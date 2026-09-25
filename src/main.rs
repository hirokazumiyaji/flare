use std::env;
use std::error::Error;
use std::fs;
use std::io::{IsTerminal, Read};
use std::time::Duration;

use firelang::{
    run_script, run_script_with_firestore, FirestoreEmulator, DEFAULT_OPERATION_TIMEOUT,
    OPERATION_TIMEOUT_ENV,
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
    let mut emulator_timeout: Option<Duration> = None;
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
            "--emulator-timeout-ms" | "--timeout-ms" => {
                let flag = args[i].clone();
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{flag} requires a value in milliseconds"))?;
                emulator_timeout = Some(parse_timeout_ms(value)?);
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

    if emulator_timeout.is_some() && backend != "emulator" {
        return Err("--emulator-timeout-ms requires --backend emulator".into());
    }

    let backend_config = match backend {
        "in-memory" => None,
        "emulator" => {
            let host = env::var("FIRESTORE_EMULATOR_HOST")
                .map_err(|_| "FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?;
            let project_id =
                env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "firelang-test".to_string());
            let operation_timeout = match emulator_timeout {
                Some(timeout) => timeout,
                None => emulator_timeout_from_env()?,
            };
            Some((host, project_id, operation_timeout))
        }
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

    let out = match backend_config {
        None => run_script(&src)?,
        Some((host, project_id, operation_timeout)) => run_script_with_firestore(
            &src,
            FirestoreEmulator::new_with_timeout(host, project_id, operation_timeout)?,
        )?,
    };

    println!("=> {}", out.inspect());
    Ok(())
}

fn parse_timeout_ms(value: &str) -> Result<Duration, Box<dyn Error>> {
    let millis = value.trim().parse::<u64>().map_err(|_| {
        "emulator timeout must be a positive integer number of milliseconds".to_string()
    })?;
    if millis == 0 {
        return Err("emulator timeout must be greater than zero".into());
    }
    Ok(Duration::from_millis(millis))
}

fn resolve_emulator_timeout(
    cli_timeout: Option<Duration>,
    env_timeout: Option<&str>,
) -> Result<Duration, Box<dyn Error>> {
    match cli_timeout {
        Some(timeout) => Ok(timeout),
        None => match env_timeout {
            Some(value) => parse_timeout_ms(value),
            None => Ok(DEFAULT_OPERATION_TIMEOUT),
        },
    }
}

fn emulator_timeout_from_env() -> Result<Duration, Box<dyn Error>> {
    match env::var(OPERATION_TIMEOUT_ENV) {
        Ok(value) => resolve_emulator_timeout(None, Some(&value)),
        Err(env::VarError::NotPresent) => resolve_emulator_timeout(None, None),
        Err(error) => Err(error.into()),
    }
}

fn print_usage() {
    eprintln!(
        "usage: fl [--backend in-memory|emulator] [--emulator-timeout-ms <ms>] (<script-file> | - | -e <script>)"
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
    eprintln!("  Emulator timeout:");
    eprintln!(
        "    --emulator-timeout-ms <ms>  per-RPC timeout (default: {}ms)",
        DEFAULT_OPERATION_TIMEOUT.as_millis()
    );
    eprintln!("    FIRESTORE_EMULATOR_TIMEOUT_MS  environment override for the default");
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_parser_rejects_zero_and_accepts_positive_values() {
        assert_eq!(parse_timeout_ms("25").unwrap(), Duration::from_millis(25));
        assert!(parse_timeout_ms("0").is_err());
        assert!(parse_timeout_ms("-1").is_err());
        assert!(parse_timeout_ms("nope").is_err());
    }

    #[test]
    fn cli_timeout_takes_precedence_over_environment_value() {
        let cli = Duration::from_millis(7);
        assert_eq!(
            resolve_emulator_timeout(Some(cli), Some("99")).unwrap(),
            cli
        );
        assert_eq!(
            resolve_emulator_timeout(None, Some("99")).unwrap(),
            Duration::from_millis(99)
        );
        assert_eq!(
            resolve_emulator_timeout(None, None).unwrap(),
            DEFAULT_OPERATION_TIMEOUT
        );
    }
}
