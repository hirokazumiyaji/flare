use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use firelang::run_script;
#[cfg(feature = "emulator")]
use firelang::{run_script_with_firestore, EmulatorConfig, FirestoreEmulator};

#[cfg(not(feature = "emulator"))]
const EMULATOR_FEATURE_ERROR: &str =
    "emulator backend requires the `emulator` Cargo feature; rebuild with --features emulator";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<OsString> = env::args_os().skip(1).collect();

    let mut backend = "in-memory";
    let mut paths: Vec<PathBuf> = vec![];
    let mut inline: Vec<String> = vec![];
    let mut use_stdin = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("--help" | "-h") => {
                print_usage();
                return Ok(());
            }
            Some("--backend") => {
                let b = args
                    .get(i + 1)
                    .ok_or("--backend requires in-memory or emulator")?;
                backend = b.to_str().ok_or("--backend value must be valid UTF-8")?;
                i += 2;
            }
            Some("-e") => {
                let script = args.get(i + 1).ok_or("-e requires a script argument")?;
                inline.push(
                    script
                        .to_str()
                        .ok_or("-e value must be valid UTF-8")?
                        .to_string(),
                );
                i += 2;
            }
            Some("-") => {
                if use_stdin {
                    return Err("stdin script given more than once".into());
                }
                use_stdin = true;
                i += 1;
            }
            Some(s) if s.starts_with('-') => {
                return Err(format!("unknown option: {s}").into());
            }
            // Anything that is not a recognised option is a script path. This
            // deliberately includes a non-UTF-8 argument (`to_str()` is
            // `None`), because such an argument is exactly the path this CLI
            // must accept rather than reject.
            Some(_) | None => {
                paths.push(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    // Validate the backend configuration before reading the script, so a
    // misconfiguration fails immediately. Only parsing and validation happen
    // here: connecting opens a socket and can block on the OS connect timeout,
    // which would stall stdin, so it is deferred until after the script is
    // read. Without the `emulator` feature there is nothing to configure, so
    // that arm reports how to enable it instead.
    #[cfg(feature = "emulator")]
    let backend_config: Option<EmulatorConfig> = match backend {
        "in-memory" => None,
        "emulator" => Some(
            FirestoreEmulator::config_from_env()?
                .ok_or("FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?,
        ),
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into());
        }
    };
    #[cfg(not(feature = "emulator"))]
    let _backend_config: Option<()> = match backend {
        "in-memory" => None,
        "emulator" => return Err(EMULATOR_FEATURE_ERROR.into()),
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into());
        }
    };
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into());
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

    #[cfg(feature = "emulator")]
    let out = match backend_config {
        None => run_script(&src)?,
        Some(config) => {
            let firestore = FirestoreEmulator::new(config.host, config.project_id)?;
            run_script_with_firestore(&src, firestore)?
        }
    };
    #[cfg(not(feature = "emulator"))]
    let out = run_script(&src)?;

    println!("=> {}", out.inspect());
    Ok(())
}

fn print_usage() {
    eprintln!("usage: fl [--backend in-memory|emulator] (<script-file> | - | -e <script>)");
    eprintln!();
    eprintln!("  Script source (exactly one):");
    eprintln!("    <script-file>  run a script file");
    eprintln!("    -             read a script from stdin");
    eprintln!("    -e <script>   run an inline script (repeatable, joined by newlines)");
    eprintln!("    (no args)     read from stdin when piped, otherwise show this usage");
    eprintln!();
    eprintln!("  Backends:");
    eprintln!("    in-memory   (default) use the in-memory Firestore stub");
    eprintln!("    emulator    (requires --features emulator) use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID");
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
