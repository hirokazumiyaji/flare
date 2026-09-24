//! SDK 接続・トークン・runtime。`EmulatorClient` は接続済みを保証するため、
//! 呼び出し側の遅延接続(`RefCell`)が不要になる。

use std::error::Error;

use firestore::{FirestoreDb, FirestoreDbOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

pub(super) struct EmulatorClient {
    pub(super) db: FirestoreDb,
    pub(super) runtime: Runtime,
    pub(super) project_id: String,
}

pub(super) fn connect(host: &str, project_id: &str) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    let db = runtime.block_on(FirestoreDb::with_options_token_source(
        options,
        gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
        emulator_token_source(),
    ))?;
    Ok(EmulatorClient {
        db,
        runtime,
        project_id: project_id.to_string(),
    })
}

fn sdk_options(host: &str, project_id: &str) -> FirestoreDbOptions {
    FirestoreDbOptions::new(project_id.to_string()).with_firebase_api_url(ensure_url_scheme(host))
}

fn ensure_url_scheme(host: &str) -> String {
    if host.contains("://") {
        host.to_string()
    } else {
        format!("http://{host}")
    }
}

fn emulator_token_source() -> gcloud_sdk::TokenSourceType {
    gcloud_sdk::TokenSourceType::ExternalSource(Box::new(
        gcloud_sdk::ExternalJwtFunctionSource::new(|| {
            std::future::ready(Ok(gcloud_sdk::Token::new(
                "Bearer".to_string(),
                "owner".into(),
                chrono::DateTime::<chrono::Utc>::from_timestamp(4_102_444_800, 0)
                    .expect("valid token expiry"),
            )))
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_options_targets_emulator_host() {
        let options = sdk_options("127.0.0.1:9", "p");
        assert_eq!(options.google_project_id, "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://127.0.0.1:9")
        );
    }

    #[test]
    fn sdk_options_keeps_explicit_scheme() {
        let options = sdk_options("http://localhost:8080", "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://localhost:8080")
        );
    }
}
