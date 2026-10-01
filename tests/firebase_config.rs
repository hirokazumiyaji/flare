use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const PRODUCTION_FIRESTORE_RULES: &str = "rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /{document=**} {
      // Fail closed until an authenticated production policy is implemented.
      allow read, write: if false;
    }
  }
}
";

const PRODUCTION_STORAGE_RULES: &str = "rules_version = '2';
service firebase.storage {
  match /b/{bucket}/o {
    // Deploy config must fail closed until an authenticated production
    // policy is implemented. Local emulator runs use storage.emulator.rules.
    match /{allPaths=**} {
      allow read, write: if false;
    }
  }
}
";

const EMULATOR_FIRESTORE_RULES: &str = "rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /{document=**} {
      // LOCAL EMULATOR ONLY. Never deploy this file to a remote project.
      allow read, write: if true;
    }
  }
}
";

const EMULATOR_STORAGE_RULES: &str = "rules_version = '2';
service firebase.storage {
  match /b/{bucket}/o {
    match /{allPaths=**} {
      // LOCAL EMULATOR ONLY. Never deploy this file to a remote project.
      allow read, write: if true;
    }
  }
}
";

fn repo_path(relative: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_repo_file(relative: &str) -> String {
    fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|err| panic!("failed to read {relative}: {err}"))
}

fn read_config(relative: &str) -> Value {
    serde_json::from_str(&read_repo_file(relative))
        .unwrap_or_else(|err| panic!("failed to parse {relative}: {err}"))
}

fn assert_rules_file(config: &Value, product: &str, expected: &str) {
    let actual = config
        .get(product)
        .and_then(|product| product.get("rules"))
        .and_then(Value::as_str);
    assert_eq!(
        actual,
        Some(expected),
        "{product}.rules must point to the expected file"
    );
}

#[test]
fn production_config_only_references_default_deny_rules() {
    let config = read_config("firebase.json");

    assert_rules_file(&config, "firestore", "firestore.rules");
    assert_rules_file(&config, "storage", "storage.rules");
    assert!(
        config.get("emulators").is_none(),
        "the default deploy config must not mix in emulator settings"
    );
    assert!(
        !repo_path(".firebaserc").exists(),
        "keep the deploy project explicit; do not check in a default active project"
    );
    assert_eq!(
        read_repo_file("firestore.rules"),
        PRODUCTION_FIRESTORE_RULES
    );
    assert_eq!(read_repo_file("storage.rules"), PRODUCTION_STORAGE_RULES);
}

#[test]
fn emulator_config_is_explicit_and_references_allow_all_rules() {
    let config = read_config("firebase.emulators.json");

    assert_rules_file(&config, "firestore", "firestore.emulator.rules");
    assert_rules_file(&config, "storage", "storage.emulator.rules");
    assert_eq!(config["emulators"]["firestore"]["host"], "127.0.0.1");
    assert_eq!(config["emulators"]["firestore"]["port"], 8080);
    assert_eq!(config["emulators"]["storage"]["host"], "127.0.0.1");
    assert_eq!(config["emulators"]["storage"]["port"], 9199);
    assert_eq!(config["emulators"]["singleProjectMode"], true);
    assert_eq!(
        read_repo_file("firestore.emulator.rules"),
        EMULATOR_FIRESTORE_RULES
    );
    assert_eq!(
        read_repo_file("storage.emulator.rules"),
        EMULATOR_STORAGE_RULES
    );
}

#[test]
fn ci_uses_the_explicit_configs_and_demo_project() {
    let ci = read_repo_file(".github/workflows/ci.yml");

    assert!(ci.contains(
        "firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-firelang"
    ));
    assert!(ci.contains(
        "firebase emulators:exec --config firebase.json --only firestore --project demo-firelang"
    ));
}

#[test]
fn workflows_do_not_deploy_firebase_rules() {
    let workflows = repo_path(".github/workflows");
    for entry in fs::read_dir(workflows).expect("workflow directory must be readable") {
        let path = entry.expect("workflow entry must be readable").path();
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        assert!(
            !contents.contains("firebase deploy"),
            "{} must not add a Firebase deployment without an explicit security review",
            path.display()
        );
    }
}
