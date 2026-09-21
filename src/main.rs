use std::env;
use std::error::Error;
use std::fs;

use firelang::{run_script, run_script_with_firestore, FirestoreEmulator};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        std::process::exit(1);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }

    let mut backend = "in-memory";
    let mut paths: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                let b = args
                    .get(i + 1)
                    .ok_or("--backend requires in-memory or emulator")?;
                backend = b.as_str();
                i += 2;
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

    if paths.len() != 1 {
        eprintln!("error: expected exactly one script file");
        print_usage();
        std::process::exit(1);
    }
    let script_path = &paths[0];
    let src = fs::read_to_string(script_path)?;

    let out = match backend {
        "in-memory" => run_script(&src)?,
        "emulator" => {
            let host = env::var("FIRESTORE_EMULATOR_HOST")
                .map_err(|_| "FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?;
            let project_id =
                env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
            run_script_with_firestore(&src, FirestoreEmulator::new(host, project_id))?
        }
        other => {
            return Err(format!("unknown backend: {other} (use in-memory or emulator)").into())
        }
    };

    println!("=> {}", out.inspect());
    Ok(())
}

fn print_usage() {
    eprintln!("usage: fl [--backend in-memory|emulator] <script-file>");
    eprintln!();
    eprintln!("  Backends:");
    eprintln!("    in-memory   (default) use the in-memory Firestore stub");
    eprintln!("    emulator    use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID");
}
