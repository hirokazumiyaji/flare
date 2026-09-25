//! SDK 接続・トークン・runtime。`EmulatorClient` は接続済みを保証するため、
//! 呼び出し側の遅延接続(`RefCell`)が不要になる。

use std::error::Error;

use firestore::{FirestoreDb, FirestoreDbOptions};
use gcloud_sdk::google::firestore::v1::GetDocumentRequest;
use gcloud_sdk::tonic::Request;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

pub(super) struct EmulatorClient {
    pub(super) db: FirestoreDb,
    pub(super) runtime: Runtime,
    pub(super) project_id: String,
}

const CONNECT_TIMEOUT_SECONDS: u64 = 5;

pub(super) fn connect(host: &str, project_id: &str) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    let db = runtime
        .block_on(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(CONNECT_TIMEOUT_SECONDS),
                FirestoreDb::with_options_token_source(
                    options,
                    gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
                    emulator_token_source(),
                ),
            )
            .await
        })
        .map_err(|_| {
            format!("Firestore emulator connection timed out after {CONNECT_TIMEOUT_SECONDS}s")
        })??;

    // The SDK's channel can be created after the TCP socket is accepted even
    // when the peer never completes the HTTP/2 handshake. Probe one harmless
    // read so `new` has a real fail-fast deadline instead of returning a
    // client that only fails on the user's first Firestore operation.
    let probe = async {
        let mut service = db.client().get();
        service
            .get_document(Request::new(GetDocumentRequest {
                name: format!(
                    "projects/{project_id}/databases/(default)/documents/__firelang_connection_probe__/probe"
                ),
                mask: None,
                consistency_selector: None,
            }))
            .await
    };
    let _ = runtime
        .block_on(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(CONNECT_TIMEOUT_SECONDS),
                probe,
            )
            .await
        })
        .map_err(|_| {
            format!(
                "Firestore emulator connection probe timed out after {CONNECT_TIMEOUT_SECONDS}s"
            )
        })?;
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
        let options = sdk_options("emulator.test", "p");
        assert_eq!(options.google_project_id, "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://emulator.test")
        );
    }

    #[test]
    fn connection_to_a_blackhole_listener_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let host = listener.local_addr().expect("listener address").to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _listener = listener;
            tx.send(connect(&host, "p").is_err()).expect("send result");
        });
        assert!(rx
            .recv_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECONDS + 2))
            .expect("connection should return after the deadline"));
    }

    #[test]
    fn sdk_options_keeps_explicit_scheme() {
        let options = sdk_options("http://emulator.test/firestore", "p");
        assert_eq!(
            options.firebase_api_url.as_deref(),
            Some("http://emulator.test/firestore")
        );
    }
}
