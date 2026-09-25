use std::env;
use std::error::Error;
use std::fs;
use std::io::{IsTerminal, Read};

use firelang::{
    run_script_with_backends, FirestoreEmulator, InMemoryFirestore, InMemoryStorage,
    StorageEmulator,
};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut backend = "in-memory";
    let mut storage_backend = "auto";
    let mut paths: Vec<String> = vec![];
    let mut inline: Vec<String> = vec![];
    let mut use_stdin = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            "--backend" => {
                let b = args
                    .get(i + 1)
                    .ok_or("--backend requires in-memory or emulator")?;
                backend = b.as_str();
                i += 2;
            }
            "--storage-backend" => {
                let b = args
                    .get(i + 1)
                    .ok_or("--storage-backend requires auto, in-memory, or emulator")?;
                storage_backend = b.as_str();
                i += 2;
            }
            "-e" => {
                let script = args.get(i + 1).ok_or("-e requires a script argument")?;
                inline.push(script.clone());
                i += 2;
            }
            "-" => {
                if use_stdin {
                    return Err("stdin script given more than once".into());
                }
                use_stdin = true;
                i += 1;
            }
            s if s.starts_with('-') => {
                return Err(format!("unknown option: {s}").into());
            }
            _ => {
                paths.push(args[i].clone());
                i += 1;
            }
        }
    }

    let firestore_config = match backend {
        "in-memory" => None,
        "emulator" => {
            let host = env::var("FIRESTORE_EMULATOR_HOST")
                .map_err(|_| "FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?;
            let project_id =
                env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
            Some((host, project_id))
        }
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into());
        }
    };

    // `auto` keeps the existing Firestore-only emulator workflow backward
    // compatible: when Firebase CLI also starts Storage, its host is picked
    // up; otherwise Storage remains in-memory. An explicit flag makes the
    // choice visible for Storage-only scripts.
    let storage_config = match storage_backend {
        "auto" if env::var("FIREBASE_STORAGE_EMULATOR_HOST").is_ok() => {
            let host = env::var("FIREBASE_STORAGE_EMULATOR_HOST").expect("checked above");
            let project_id = env::var("FIREBASE_PROJECT_ID")
                .or_else(|_| env::var("GCLOUD_PROJECT"))
                .or_else(|_| env::var("FIRESTORE_PROJECT_ID"))
                .unwrap_or_else(|_| "firelang-test".to_string());
            Some((host, project_id))
        }
        "auto" | "in-memory" => None,
        "emulator" => {
            let host = env::var("FIREBASE_STORAGE_EMULATOR_HOST").map_err(|_| {
                "FIREBASE_STORAGE_EMULATOR_HOST must be set when using --storage-backend emulator"
            })?;
            let project_id = env::var("FIREBASE_PROJECT_ID")
                .or_else(|_| env::var("GCLOUD_PROJECT"))
                .or_else(|_| env::var("FIRESTORE_PROJECT_ID"))
                .unwrap_or_else(|_| "firelang-test".to_string());
            Some((host, project_id))
        }
        other => {
            return Err(format!(
                "unknown storage backend: {other} (use auto, in-memory, or emulator)"
            )
            .into());
        }
    };

    let src = if !inline.is_empty() {
        if !paths.is_empty() || use_stdin {
            eprintln!("error: -e cannot be combined with a script file or stdin");
            print_usage();
            std::process::exit(1);
        }
        // 複数の -e は改行で連結する(Ruby 互換)。
        inline.join("\n")
    } else if use_stdin {
        if !paths.is_empty() {
            eprintln!("error: cannot read from both a script file and stdin");
            print_usage();
            std::process::exit(1);
        }
        read_stdin()?
    } else if paths.len() == 1 {
        fs::read_to_string(&paths[0])?
    } else if paths.is_empty() && !std::io::stdin().is_terminal() {
        // 引数なし + パイプ時は stdin から読む。端末直結の引数なしは usage エラー。
        read_stdin()?
    } else {
        if paths.is_empty() {
            print_usage();
        } else {
            eprintln!("error: expected exactly one script file");
            print_usage();
        }
        std::process::exit(1);
    };

    let out = match (firestore_config, storage_config) {
        (None, None) => run_script_with_backends(
            &src,
            InMemoryFirestore::default(),
            InMemoryStorage::default(),
        )?,
        (Some((host, project_id)), None) => run_script_with_backends(
            &src,
            FirestoreEmulator::new(host, project_id)?,
            InMemoryStorage::default(),
        )?,
        (None, Some((host, project_id))) => run_script_with_backends(
            &src,
            InMemoryFirestore::default(),
            make_storage(host, project_id)?,
        )?,
        (Some((firestore_host, firestore_project)), Some((storage_host, storage_project))) => {
            run_script_with_backends(
                &src,
                FirestoreEmulator::new(firestore_host, firestore_project)?,
                make_storage(storage_host, storage_project)?,
            )?
        }
    };

    println!("=> {}", out.inspect());
    Ok(())
}

fn make_storage(host: String, project_id: String) -> Result<StorageEmulator, Box<dyn Error>> {
    let mut storage = StorageEmulator::new(host, project_id)?;
    if let Ok(bucket) = env::var("FIREBASE_STORAGE_BUCKET") {
        storage = storage.with_bucket(bucket)?;
    }
    if let Ok(token) = env::var("FIREBASE_STORAGE_AUTH_TOKEN") {
        storage = storage.with_auth_token(token)?;
    }
    Ok(storage)
}

fn print_usage() {
    eprintln!(
        "usage: fl [--backend in-memory|emulator] [--storage-backend auto|in-memory|emulator] (<script-file> | - | -e <script>)"
    );
    eprintln!();
    eprintln!("  Script source (exactly one):");
    eprintln!("    <script-file>  run a script file");
    eprintln!("    -             read a script from stdin");
    eprintln!("    -e <script>   run an inline script (repeatable, joined by newlines)");
    eprintln!("    (no args)     read from stdin when piped, otherwise show this usage");
    eprintln!();
    eprintln!("  Backends:");
    eprintln!("    in-memory   (default) use the in-memory Firestore stub");
    eprintln!("    emulator    use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID");
    eprintln!();
    eprintln!("  Storage backends:");
    eprintln!("    auto       use the Storage emulator when FIREBASE_STORAGE_EMULATOR_HOST is set");
    eprintln!("    in-memory  (default when auto has no host) use the in-memory Storage stub");
    eprintln!("    emulator   require FIREBASE_STORAGE_EMULATOR_HOST");
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
