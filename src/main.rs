use std::env;
use std::error::Error;
use std::fs;
use std::io::{IsTerminal, Read};

use std::time::Duration;

use firelang::{FirestoreEmulator, Runtime, RuntimeLimits};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut backend = "in-memory";
    let mut paths: Vec<String> = vec![];
    let mut inline: Vec<String> = vec![];
    let mut use_stdin = false;
    let mut limits = RuntimeLimits::default();
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
            "--max-call-depth" => {
                limits.max_call_depth = Some(parse_u64(&args, i, "--max-call-depth")? as usize);
                i += 2;
            }
            "--max-steps" => {
                limits.max_steps = Some(parse_u64(&args, i, "--max-steps")?);
                i += 2;
            }
            "--max-loop-iterations" => {
                limits.max_loop_iterations = Some(parse_u64(&args, i, "--max-loop-iterations")?);
                i += 2;
            }
            "--max-wall-clock-ms" => {
                limits.max_wall_clock = Some(Duration::from_millis(parse_u64(
                    &args,
                    i,
                    "--max-wall-clock-ms",
                )?));
                i += 2;
            }
            "--max-parser-depth" => {
                limits.max_parser_depth = Some(parse_u64(&args, i, "--max-parser-depth")? as usize);
                i += 2;
            }
            "--max-value-depth" => {
                limits.max_value_depth = Some(parse_u64(&args, i, "--max-value-depth")? as usize);
                i += 2;
            }
            "--max-query-results" => {
                limits.max_query_results =
                    Some(parse_u64(&args, i, "--max-query-results")? as usize);
                i += 2;
            }
            "--unlimited" => {
                limits = RuntimeLimits::unlimited();
                i += 1;
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

    let backend_config = match backend {
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
        None => Runtime::with_limits(firelang::InMemoryFirestore::default(), limits).run(&src)?,
        Some((host, project_id)) => {
            let fs = FirestoreEmulator::new(host, project_id)?;
            Runtime::with_limits(fs, limits).run(&src)?
        }
    };

    println!("=> {}", out.inspect());
    Ok(())
}

/// Parse a non-negative budget value for `--<name> <n>`.
fn parse_u64(args: &[String], i: usize, name: &str) -> Result<u64, Box<dyn Error>> {
    let raw = args
        .get(i + 1)
        .ok_or_else(|| format!("{name} requires a number"))?;
    raw.parse::<u64>()
        .map_err(|_| format!("{name} requires a non-negative integer, got {raw:?}").into())
}

fn print_usage() {
    eprintln!(
        "usage: fl [--backend in-memory|emulator] [budget options] (<script-file> | - | -e <script>)"
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
    eprintln!("  Budgets (all optional; finite defaults protect untrusted input):");
    eprintln!("    --max-call-depth <n>        nested call depth (default 64)");
    eprintln!("    --max-steps <n>             evaluated statements (default 1000000)");
    eprintln!("    --max-loop-iterations <n>   while/each/map iterations (default 100000)");
    eprintln!("    --max-wall-clock-ms <n>     wall-clock budget in ms (default 5000)");
    eprintln!("    --max-parser-depth <n>      parser/AST nesting (default 128)");
    eprintln!("    --max-value-depth <n>       runtime Value nesting (default 64)");
    eprintln!("    --max-query-results <n>     query candidates materialized (default 10000)");
    eprintln!("    --unlimited                 disable every budget (trusted scripts only;");
    eprintln!("                                recursion can then overflow the native stack)");
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
