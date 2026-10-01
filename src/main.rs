use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
#[cfg(feature = "emulator")]
use std::time::Duration;

use firelang::{
    run_script_with_all_backends, Auth, InMemoryAuth, InMemoryFirestore, InMemoryStorage,
    StorageEmulator,
};
#[cfg(feature = "emulator")]
use firelang::{
    parse_timeout_ms as parse_library_timeout_ms, AuthEmulator, EmulatorConfig, FirestoreEmulator,
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
                let flag = args[i].to_str().unwrap_or("--emulator-timeout-ms").to_string();
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
                let value = b.to_str().ok_or("--auth-backend value must be valid UTF-8")?;
                if value != "in-memory" && value != "emulator" {
                    return Err(format!(
                        "unknown auth backend: {value} (use in-memory or emulator)"
                    )
                    .into());
                }
                auth_backend = value.to_string();
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

    // `auto` keeps the existing Firestore-only emulator workflow backward
    // compatible: when Firebase CLI also starts Storage, its host is picked
    // up; otherwise Storage remains in-memory. An explicit flag makes the
    // choice visible for Storage-only scripts.
    let storage_config = match storage_backend {
        // Auto mode is deliberately best-effort. A stale host variable (or
        // a non-UTF-8 value) must not make Firestore-only scripts fail before
        // they ever use Storage.
        "auto" => match env::var("FIREBASE_STORAGE_EMULATOR_HOST") {
            Ok(host) => Some((host, storage_project_id())),
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
            Some((host, storage_project_id()))
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
    let selected_auth = select_auth(&auth_backend)?;
    #[cfg(feature = "emulator")]
    let out = match (backend_config, selected_storage) {
        (None, SelectedStorage::Memory) => run_script_with_all_backends(&src, 
            InMemoryFirestore::default(),
            InMemoryStorage::default(),
            selected_auth,
        )?,
        (Some(config), SelectedStorage::Memory) => run_script_with_all_backends(&src, 
            FirestoreEmulator::new_with_timeout(
                config.host,
                config.project_id,
                emulator_timeout.unwrap_or(config.operation_timeout),
            )?,
            InMemoryStorage::default(),
            selected_auth,
        )?,
        (None, SelectedStorage::Emulator(storage)) => {
            run_script_with_all_backends(&src, InMemoryFirestore::default(), storage, selected_auth)?
        }
        (Some(config), SelectedStorage::Emulator(storage)) => {
            run_script_with_all_backends(&src, 
                FirestoreEmulator::new_with_timeout(
                    config.host,
                    config.project_id,
                    emulator_timeout.unwrap_or(config.operation_timeout),
                )?,
                storage,
                selected_auth,
            )?
        }
    };
    #[cfg(not(feature = "emulator"))]
    let out = match selected_storage {
        SelectedStorage::Memory => run_script_with_all_backends(&src, 
            InMemoryFirestore::default(),
            InMemoryStorage::default(),
            selected_auth,
        )?,
        SelectedStorage::Emulator(storage) => {
            run_script_with_all_backends(&src, InMemoryFirestore::default(), storage, selected_auth)?
        }
    };

    println!("=> {}", out.inspect());
    Ok(())
}

#[cfg(feature = "emulator")]
fn parse_timeout_ms(value: &str) -> Result<Duration, Box<dyn Error>> {
    // Reuse the library parser so the CLI and FirestoreEmulator::from_env
    // cannot disagree about what a valid timeout is.
    parse_library_timeout_ms(value)
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

fn storage_project_id() -> String {
    env::var("FIREBASE_PROJECT_ID")
        .or_else(|_| env::var("GCLOUD_PROJECT"))
        .or_else(|_| env::var("FIRESTORE_PROJECT_ID"))
        .unwrap_or_else(|_| "firelang-test".to_string())
}

fn select_storage(
    mode: &str,
    config: Option<(String, String)>,
) -> Result<SelectedStorage, Box<dyn Error>> {
    let Some((host, project_id)) = config else {
        return Ok(SelectedStorage::Memory);
    };
    match make_storage(host, project_id) {
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
fn select_auth(mode: &str) -> Result<Box<dyn Auth>, Box<dyn Error>> {
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
                return Ok(Box::new(AuthEmulator::new(
                    host,
                    project_id_from_env(true),
                )?));
            }
            #[cfg(not(feature = "emulator"))]
            {
                let _ = host;
                return Err(EMULATOR_FEATURE_ERROR.into());
            }
        }
        other => Err(format!("unknown auth backend: {other} (use in-memory or emulator)").into()),
    }
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
        "usage: fl [--backend in-memory|emulator] [--storage-backend auto|in-memory|emulator] [--auth-backend in-memory|emulator] [--emulator-timeout-ms <ms>] (<script-file> | - | -e <script>)"
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
    eprintln!("    emulator    use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID");
    #[cfg(not(feature = "emulator"))]
    eprintln!(
        "    emulator    ({EMULATOR_FEATURE_ERROR}) use FIRESTORE_EMULATOR_HOST and optional FIRESTORE_PROJECT_ID"
    );
    #[cfg(feature = "emulator")]
    eprintln!();
    #[cfg(feature = "emulator")]
    eprintln!("  Emulator timeout:");
    #[cfg(feature = "emulator")]
    eprintln!(
        "    --emulator-timeout-ms <ms>  per-RPC timeout (default: {}ms)",
        firelang::DEFAULT_OPERATION_TIMEOUT.as_millis()
    );
    #[cfg(feature = "emulator")]
    eprintln!("    --timeout-ms <ms>          alias for --emulator-timeout-ms");
    #[cfg(feature = "emulator")]
    eprintln!("    FIRESTORE_EMULATOR_TIMEOUT_MS  environment override for the default");
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
#[cfg(feature = "emulator")]
fn project_id_from_env(prefer_firebase: bool) -> String {
    resolve_project_id(prefer_firebase, |key| env::var(key).ok())
}

/// Pure precedence rule behind [`project_id_from_env`], split out so it can be
/// tested without mutating process-wide environment state.
#[cfg(feature = "emulator")]
fn resolve_project_id(prefer_firebase: bool, lookup: impl Fn(&str) -> Option<String>) -> String {
    let (first, second) = if prefer_firebase {
        ("FIREBASE_PROJECT_ID", "FIRESTORE_PROJECT_ID")
    } else {
        ("FIRESTORE_PROJECT_ID", "FIREBASE_PROJECT_ID")
    };
    lookup(first)
        .or_else(|| lookup(second))
        .unwrap_or_else(|| "firelang-test".to_string())

}

fn read_stdin() -> Result<String, Box<dyn Error>> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(all(test, feature = "emulator"))]
mod tests {
    use super::resolve_project_id;

    fn lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn firestore_backend_keeps_firestore_project_id_precedence() {
        let both = lookup(&[
            ("FIRESTORE_PROJECT_ID", "from-firestore"),
            ("FIREBASE_PROJECT_ID", "from-firebase"),
        ]);
        assert_eq!(resolve_project_id(false, both), "from-firestore");
    }

    #[test]
    fn auth_backend_prefers_firebase_project_id() {
        let both = lookup(&[
            ("FIRESTORE_PROJECT_ID", "from-firestore"),
            ("FIREBASE_PROJECT_ID", "from-firebase"),
        ]);
        assert_eq!(resolve_project_id(true, both), "from-firebase");
    }

    #[test]
    fn each_backend_falls_back_to_the_other_name() {
        let only_firebase = lookup(&[("FIREBASE_PROJECT_ID", "shared")]);
        assert_eq!(resolve_project_id(false, only_firebase), "shared");
        let only_firestore = lookup(&[("FIRESTORE_PROJECT_ID", "shared")]);
        assert_eq!(resolve_project_id(true, only_firestore), "shared");
    }

    #[test]
    fn default_project_id_is_used_when_unset() {
        let empty = lookup(&[]);
        assert_eq!(resolve_project_id(false, &empty), "firelang-test");
        assert_eq!(resolve_project_id(true, &empty), "firelang-test");
    }
}
