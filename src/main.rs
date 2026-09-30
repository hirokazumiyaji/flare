use std::env;
use std::error::Error;
use std::fs;
use std::io::{IsTerminal, Read};

use firelang::{
    run_script, run_script_with_auth, run_script_with_backends, run_script_with_firestore,
    AuthEmulator, FirestoreEmulator,
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
    let mut auth_backend = "in-memory";
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
            "--auth-backend" => {
                let b = args
                    .get(i + 1)
                    .ok_or("--auth-backend requires in-memory or emulator")?;
                auth_backend = b.as_str();
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

    // Backend / auth-backend の検証はスクリプト入力(stdin/file)より前に行う。
    let backend_config = match backend {
        "in-memory" => None,
        "emulator" => Some((
            env::var("FIRESTORE_EMULATOR_HOST")
                .map_err(|_| "FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?,
            project_id_from_env(false),
        )),
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into());
        }
    };
    let auth_config = match auth_backend {
        "in-memory" => None,
        "emulator" => Some((
            env::var("FIREBASE_AUTH_EMULATOR_HOST").map_err(|_| {
                "FIREBASE_AUTH_EMULATOR_HOST must be set when using --auth-backend emulator"
            })?,
            project_id_from_env(true),
        )),
        other => {
            return Err(
                format!("unknown auth backend: {other} (use in-memory or emulator)").into(),
            );
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

    let out = match (backend_config, auth_config) {
        (None, None) => run_script(&src)?,
        (Some((host, project_id)), None) => {
            run_script_with_firestore(&src, FirestoreEmulator::new(host, project_id)?)?
        }
        (None, Some((host, project_id))) => {
            run_script_with_auth(&src, AuthEmulator::new(host, project_id)?)?
        }
        (Some((host, project_id)), Some((auth_host, auth_project_id))) => run_script_with_backends(
            &src,
            FirestoreEmulator::new(host, project_id)?,
            AuthEmulator::new(auth_host, auth_project_id)?,
        )?,
    };

    println!("=> {}", out.inspect());
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: fl [--backend in-memory|emulator] [--auth-backend in-memory|emulator] (<script-file> | - | -e <script>)"
    );
    eprintln!();
    eprintln!("  Script source (exactly one):");
    eprintln!("    <script-file>  run a script file");
    eprintln!("    -             read a script from stdin");
    eprintln!("    -e <script>   run an inline script (repeatable, joined by newlines)");
    eprintln!("    (no args)     read from stdin when piped, otherwise show this usage");
    eprintln!();
    eprintln!("  Firestore backend:");
    eprintln!("    in-memory   (default) use the in-memory Firestore stub");
    eprintln!("    emulator    use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID");
    eprintln!();
    eprintln!("  Auth backend:");
    eprintln!("    in-memory   (default) use the in-memory Auth stub");
    eprintln!("    emulator    use FIREBASE_AUTH_EMULATOR_HOST and optional FIREBASE_PROJECT_ID");
}

/// Resolve the Firebase project ID from the environment.
///
/// Each backend keeps its documented precedence so existing Firestore setups
/// are unaffected:
/// - Firestore (`prefer_firebase = false`): `FIRESTORE_PROJECT_ID`,
///   then `FIREBASE_PROJECT_ID`, then `firelang-test`.
/// - Auth (`prefer_firebase = true`): `FIREBASE_PROJECT_ID`,
///   then `FIRESTORE_PROJECT_ID`, then `firelang-test`.
///
/// Reading both names lets one Firebase CLI project (`--project X`) drive the
/// Firestore and Auth emulators together without exporting a second variable.
fn project_id_from_env(prefer_firebase: bool) -> String {
    let (first, second) = if prefer_firebase {
        ("FIREBASE_PROJECT_ID", "FIRESTORE_PROJECT_ID")
    } else {
        ("FIRESTORE_PROJECT_ID", "FIREBASE_PROJECT_ID")
    };
    env::var(first)
        .or_else(|_| env::var(second))
        .unwrap_or_else(|_| "firelang-test".to_string())
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
