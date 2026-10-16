use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(feature = "emulator")]
use flare::AuthEmulatorOptions;
#[cfg(feature = "emulator")]
use flare::{
    parse_timeout_ms as parse_library_timeout_ms, AuthEmulator, EmulatorConfig, FirestoreEmulator,
};
use flare::{
    run_script_with_services_and_limits, Auth, Firestore, InMemoryAuth, InMemoryFirestore,
    InMemoryStorage, ProjectConfig, RuntimeLimits, Services, Storage, StorageEmulator,
};

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
    let mut storage_backend = "auto";
    let mut auth_backend = "in-memory".to_string();
    #[cfg(feature = "emulator")]
    let mut emulator_timeout: Option<Duration> = None;
    #[cfg(feature = "emulator")]
    let mut emulator_timeout_flag: Option<String> = None;
    let mut paths: Vec<PathBuf> = vec![];
    let mut inline: Vec<String> = vec![];
    let mut use_stdin = false;
    // Every budget is finite by default: `fl` accepts arbitrary scripts, so an
    // unbounded recursion, loop, or nested value must be a reported error
    // rather than a hang or a stack overflow.
    let mut limits = RuntimeLimits::default();
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

            Some("--storage-backend") => {
                let b = args
                    .get(i + 1)
                    .ok_or("--storage-backend requires auto, in-memory, or emulator")?;
                storage_backend = b
                    .to_str()
                    .ok_or("--storage-backend value must be valid UTF-8")?;
                i += 2;
            }
            #[cfg(feature = "emulator")]
            Some("--emulator-timeout-ms") | Some("--timeout-ms") => {
                let flag = args[i]
                    .to_str()
                    .unwrap_or("--emulator-timeout-ms")
                    .to_string();
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{flag} requires a value in milliseconds"))?;
                let value = value
                    .to_str()
                    .ok_or_else(|| format!("{flag} value must be valid UTF-8"))?;
                emulator_timeout = Some(parse_timeout_ms(value)?);
                emulator_timeout_flag = Some(flag);
                i += 2;
            }
            Some("--auth-backend") => {
                let b = args
                    .get(i + 1)
                    .ok_or("--auth-backend requires in-memory or emulator")?;
                let value = b
                    .to_str()
                    .ok_or("--auth-backend value must be valid UTF-8")?;
                if value != "in-memory" && value != "emulator" {
                    return Err(format!(
                        "unknown auth backend: {value} (use in-memory or emulator)"
                    )
                    .into());
                }
                auth_backend = value.to_string();
                i += 2;
            }
            Some("--max-call-depth") => {
                limits.max_call_depth = Some(parse_u64(&args, i, "--max-call-depth")? as usize);
                i += 2;
            }
            Some("--max-steps") => {
                limits.max_steps = Some(parse_u64(&args, i, "--max-steps")?);
                i += 2;
            }
            Some("--max-loop-iterations") => {
                limits.max_loop_iterations = Some(parse_u64(&args, i, "--max-loop-iterations")?);
                i += 2;
            }
            Some("--max-wall-clock-ms") => {
                limits.max_wall_clock = Some(Duration::from_millis(parse_u64(
                    &args,
                    i,
                    "--max-wall-clock-ms",
                )?));
                i += 2;
            }
            Some("--max-parser-depth") => {
                limits.max_parser_depth = Some(parse_u64(&args, i, "--max-parser-depth")? as usize);
                i += 2;
            }
            Some("--max-value-depth") => {
                limits.max_value_depth = Some(parse_u64(&args, i, "--max-value-depth")? as usize);
                i += 2;
            }
            Some("--max-query-results") => {
                limits.max_query_results =
                    Some(parse_u64(&args, i, "--max-query-results")? as usize);
                i += 2;
            }
            Some("--max-string-bytes") => {
                limits.max_string_bytes =
                    Some(parse_u64(&args, i, "--max-string-bytes")? as usize);
                i += 2;
            }
            Some("--max-bytes-size") => {
                limits.max_bytes_size =
                    Some(parse_u64(&args, i, "--max-bytes-size")? as usize);
                i += 2;
            }
            Some("--unlimited") => {
                limits = RuntimeLimits::unlimited();
                i += 1;
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

    let project = resolve_project_config(|key| env::var(key).ok())?;

    #[cfg(feature = "emulator")]
    if emulator_timeout.is_some() && backend != "emulator" {
        let flag = emulator_timeout_flag
            .as_deref()
            .unwrap_or("--emulator-timeout-ms");
        return Err(format!("{flag} requires --backend emulator").into());
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
        "emulator" => {
            let host = env::var("FIRESTORE_EMULATOR_HOST")
                .map_err(|_| "FIRESTORE_EMULATOR_HOST must be set when using --backend emulator")?;
            Some(EmulatorConfig {
                host,
                project_id: project.project_id.clone().unwrap(),
                operation_timeout: match emulator_timeout {
                    Some(timeout) => timeout,
                    None => timeout_from_env()?,
                },
            })
        }

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

    // `auto` keeps the existing Firestore-only emulator workflow backward
    // compatible: when Firebase CLI also starts Storage, its host is picked
    // up; otherwise Storage remains in-memory. An explicit flag makes the
    // choice visible for Storage-only scripts.
    let storage_config = match storage_backend {
        // Auto mode is deliberately best-effort. A stale host variable (or
        // a non-UTF-8 value) must not make Firestore-only scripts fail before
        // they ever use Storage.
        "auto" => match env::var("FIREBASE_STORAGE_EMULATOR_HOST") {
            Ok(host) => Some((
                host,
                project.project_id.clone().unwrap(),
                project.storage_bucket.clone(),
            )),
            Err(_) => None,
        },
        "in-memory" => None,
        "emulator" => {
            let host = match env::var("FIREBASE_STORAGE_EMULATOR_HOST") {
                Ok(host) => host,
                Err(env::VarError::NotPresent) => {
                    return Err(
                        "FIREBASE_STORAGE_EMULATOR_HOST must be set when using --storage-backend emulator"
                            .into(),
                    );
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err("FIREBASE_STORAGE_EMULATOR_HOST must be valid UTF-8".into());
                }
            };
            Some((
                host,
                project.project_id.clone().unwrap(),
                project.storage_bucket.clone(),
            ))
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

    let selected_storage = select_storage(storage_backend, storage_config)?;
    let selected_auth = select_auth(&auth_backend, project.project_id.as_deref().unwrap())?;
    let storage: Box<dyn Storage> = match selected_storage {
        SelectedStorage::Memory => Box::new(InMemoryStorage::default()),
        SelectedStorage::Emulator(storage) => Box::new(storage),
    };
    #[cfg(feature = "emulator")]
    let mut firestore: Box<dyn Firestore> = Box::new(InMemoryFirestore::default());
    #[cfg(not(feature = "emulator"))]
    let firestore: Box<dyn Firestore> = Box::new(InMemoryFirestore::default());
    #[cfg(feature = "emulator")]
    if let Some(config) = backend_config {
        firestore = Box::new(FirestoreEmulator::new_with_timeout(
            config.host,
            config.project_id,
            Some(emulator_timeout.unwrap_or(config.operation_timeout)),
        )?);
    }
    let services = Services::in_memory()
        .with_firestore(firestore)
        .with_storage(storage)
        .with_auth(selected_auth)
        .with_project(project);
    let out = run_script_with_services_and_limits(&src, services, limits)?;

    println!("=> {}", out.inspect());
    Ok(())
}

/// Parse a non-negative budget value for `--<name> <n>`.
fn parse_u64(args: &[OsString], i: usize, name: &str) -> Result<u64, Box<dyn Error>> {
    let raw = args
        .get(i + 1)
        .ok_or_else(|| format!("{name} requires a number"))?;
    raw.to_str()
        .ok_or_else(|| format!("{name} value must be valid UTF-8"))?
        .parse::<u64>()
        .map_err(|_| format!("{name} requires a non-negative integer").into())
}

#[cfg(feature = "emulator")]
fn parse_timeout_ms(value: &str) -> Result<Duration, Box<dyn Error>> {
    // Reuse the library parser so the CLI and FirestoreEmulator::from_env
    // cannot disagree about what a valid timeout is.
    parse_library_timeout_ms(value)
}

#[cfg(feature = "emulator")]
fn timeout_from_env() -> Result<Duration, Box<dyn Error>> {
    match env::var("FIRESTORE_EMULATOR_TIMEOUT_MS") {
        Ok(value) => parse_timeout_ms(&value),
        Err(env::VarError::NotPresent) => Ok(flare::DEFAULT_OPERATION_TIMEOUT),
        Err(env::VarError::NotUnicode(_)) => {
            Err("FIRESTORE_EMULATOR_TIMEOUT_MS must be valid UTF-8".into())
        }
    }
}

#[cfg(all(test, feature = "emulator"))]
mod timeout_helper_tests {
    use super::*;

    #[test]
    fn timeout_parser_rejects_zero_and_accepts_positive_values() {
        assert_eq!(parse_timeout_ms("25").unwrap(), Duration::from_millis(25));
        assert!(parse_timeout_ms("0").is_err());
        assert!(parse_timeout_ms("-1").is_err());
        assert!(parse_timeout_ms("nope").is_err());
    }
}

enum SelectedStorage {
    Memory,
    Emulator(StorageEmulator),
}

fn select_storage(
    mode: &str,
    config: Option<(String, String, Option<String>)>,
) -> Result<SelectedStorage, Box<dyn Error>> {
    let Some((host, project_id, bucket)) = config else {
        return Ok(SelectedStorage::Memory);
    };
    match make_storage(host, project_id, bucket) {
        Ok(storage) => Ok(SelectedStorage::Emulator(storage)),
        Err(error) if mode == "auto" => {
            eprintln!("warning: storage emulator unavailable ({error}); using in-memory");
            Ok(SelectedStorage::Memory)
        }
        Err(error) => Err(error),
    }
}

/// Resolve `--auth-backend`. Unlike Storage's best-effort `auto` mode there
/// is no ambient detection: `emulator` requires `FIREBASE_AUTH_EMULATOR_HOST`
/// and fails loudly when it is missing or invalid.
fn select_auth(mode: &str, _project_id: &str) -> Result<Box<dyn Auth>, Box<dyn Error>> {
    match mode {
        "in-memory" => Ok(Box::new(InMemoryAuth::default())),
        "emulator" => {
            let host = match env::var("FIREBASE_AUTH_EMULATOR_HOST") {
                Ok(host) => host,
                Err(env::VarError::NotPresent) => {
                    return Err(
                        "FIREBASE_AUTH_EMULATOR_HOST must be set when using --auth-backend emulator"
                            .into(),
                    );
                }
                Err(env::VarError::NotUnicode(_)) => {
                    return Err("FIREBASE_AUTH_EMULATOR_HOST must be valid UTF-8".into());
                }
            };
            #[cfg(feature = "emulator")]
            {
                Ok(Box::new(AuthEmulator::new_with_options(
                    host,
                    _project_id,
                    AuthEmulatorOptions {
                        api_key: env::var("FIREBASE_API_KEY")
                            .unwrap_or_else(|_| "fake-api-key".into()),
                        ..AuthEmulatorOptions::default()
                    },
                )?))
            }
            #[cfg(not(feature = "emulator"))]
            {
                let _ = host;
                Err(EMULATOR_FEATURE_ERROR.into())
            }
        }
        other => Err(format!("unknown auth backend: {other} (use in-memory or emulator)").into()),
    }
}

fn make_storage(
    host: String,
    project_id: String,
    bucket: Option<String>,
) -> Result<StorageEmulator, Box<dyn Error>> {
    let mut storage = StorageEmulator::new(host, project_id)?;
    if let Some(bucket) = bucket {
        storage = storage.with_bucket(bucket)?;
    }
    if let Ok(token) = env::var("FIREBASE_STORAGE_AUTH_TOKEN") {
        storage = storage.with_auth_token(token)?;
    }
    Ok(storage)
}

fn resolve_project_config(
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<ProjectConfig, Box<dyn Error>> {
    let mut values = Vec::new();
    for key in [
        "FIREBASE_PROJECT_ID",
        "GCLOUD_PROJECT",
        "FIRESTORE_PROJECT_ID",
    ] {
        if let Some(value) = lookup(key) {
            if value.trim().is_empty() {
                return Err(format!("{key} must not be empty").into());
            }
            values.push((key, value));
        }
    }
    if let Some((_, first)) = values.first() {
        if values.iter().any(|(_, value)| value != first) {
            return Err("Firebase project environment variables have conflicting values".into());
        }
    }
    let project_id = values
        .first()
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "flare-test".into());
    let storage_bucket = lookup("FIREBASE_STORAGE_BUCKET");
    Ok(ProjectConfig {
        project_id: Some(project_id),
        storage_bucket,
    })
}

fn print_usage() {
    eprintln!(
        "usage: fl [--backend in-memory|emulator] [--storage-backend auto|in-memory|emulator] [--auth-backend in-memory|emulator] [--emulator-timeout-ms <ms>] [budget options] (<script-file> | - | -e <script>)"
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
    // Keep the help honest about this build's own capabilities: a binary
    // compiled with the feature enabled does accept --backend emulator, and
    // telling its users otherwise makes --help contradict the artifact.
    #[cfg(feature = "emulator")]
    eprintln!("    emulator    use FIRESTORE_EMULATOR_HOST and shared Firebase project settings");
    #[cfg(not(feature = "emulator"))]
    eprintln!(
        "    emulator    ({EMULATOR_FEATURE_ERROR}) use FIRESTORE_EMULATOR_HOST and shared Firebase project settings"
    );
    #[cfg(feature = "emulator")]
    eprintln!();
    #[cfg(feature = "emulator")]
    eprintln!("  Emulator timeout:");
    #[cfg(feature = "emulator")]
    eprintln!(
        "    --emulator-timeout-ms <ms>  per-RPC timeout (default: {}ms)",
        flare::DEFAULT_OPERATION_TIMEOUT.as_millis()
    );
    #[cfg(feature = "emulator")]
    eprintln!("    --timeout-ms <ms>          alias for --emulator-timeout-ms");
    #[cfg(feature = "emulator")]
    eprintln!("    FIRESTORE_EMULATOR_TIMEOUT_MS  environment override for the default");
    eprintln!();
    eprintln!("  Budgets (all optional; finite defaults protect untrusted input):");
    eprintln!(
        "    --max-call-depth <n>        nested call depth (default {})",
        flare::DEFAULT_MAX_CALL_DEPTH
    );
    eprintln!(
        "    --max-steps <n>             evaluated statements (default {})",
        flare::DEFAULT_MAX_STEPS
    );
    eprintln!(
        "    --max-loop-iterations <n>   while/each/map iterations (default {})",
        flare::DEFAULT_MAX_LOOP_ITERATIONS
    );
    eprintln!(
        "    --max-wall-clock-ms <n>     wall-clock budget in ms (default {})",
        flare::DEFAULT_MAX_WALL_CLOCK_MS
    );
    eprintln!(
        "    --max-parser-depth <n>      parser/AST nesting (default {})",
        flare::DEFAULT_MAX_PARSER_DEPTH
    );
    eprintln!(
        "    --max-value-depth <n>       runtime Value nesting (default {})",
        flare::DEFAULT_MAX_VALUE_DEPTH
    );
    eprintln!(
        "    --max-query-results <n>     query candidates materialized (default {})",
        flare::DEFAULT_MAX_QUERY_RESULTS
    );
    eprintln!(
        "    --max-string-bytes <n>      maximum string size in bytes (default {})",
        flare::DEFAULT_MAX_STRING_BYTES
    );
    eprintln!(
        "    --max-bytes-size <n>        maximum bytes size in bytes (default {})",
        flare::DEFAULT_MAX_BYTES_SIZE
    );
    eprintln!("    --unlimited                 disable every budget (trusted scripts only;");
    eprintln!("                                recursion can then overflow the native stack)");
    eprintln!();
    eprintln!("  Storage backends:");
    eprintln!("    auto       use the Storage emulator when FIREBASE_STORAGE_EMULATOR_HOST is set");
    eprintln!("    in-memory  (default when auto has no host) use the in-memory Storage stub");
    eprintln!("    emulator   require FIREBASE_STORAGE_EMULATOR_HOST");
    eprintln!();
    eprintln!("  Auth backends:");
    eprintln!("    in-memory  (default) use the in-memory Auth stub");
    eprintln!("    emulator   require FIREBASE_AUTH_EMULATOR_HOST");
    eprintln!();
    eprintln!("  Auth backend:");
    eprintln!("    in-memory   (default) use the in-memory Auth stub");
    eprintln!(
        "    emulator    use FIREBASE_AUTH_EMULATOR_HOST and shared Firebase project settings"
    );
}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod project_config_tests {
    use super::*;

    #[test]
    fn project_variables_must_agree_and_default_to_flare_test() {
        let empty = resolve_project_config(|_| None).unwrap();
        assert_eq!(empty.project_id.as_deref(), Some("flare-test"));
        for variable in [
            "FIREBASE_PROJECT_ID",
            "GCLOUD_PROJECT",
            "FIRESTORE_PROJECT_ID",
        ] {
            let config =
                resolve_project_config(|key| (key == variable).then(|| "demo".into())).unwrap();
            assert_eq!(config.project_id.as_deref(), Some("demo"), "{variable}");
        }
        let same = resolve_project_config(|key| match key {
            "FIREBASE_PROJECT_ID" | "GCLOUD_PROJECT" => Some("demo".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(same.project_id.as_deref(), Some("demo"));
        assert!(resolve_project_config(|key| match key {
            "FIREBASE_PROJECT_ID" => Some("one".into()),
            "FIRESTORE_PROJECT_ID" => Some("two".into()),
            _ => None,
        })
        .is_err());
    }

    #[test]
    fn explicit_storage_bucket_is_kept_in_project_config() {
        let config = resolve_project_config(|key| match key {
            "FIREBASE_STORAGE_BUCKET" => Some("custom-bucket".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.storage_bucket.as_deref(), Some("custom-bucket"));
    }
}
