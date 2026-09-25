//! SDK 接続・トークン・runtime。`EmulatorClient` は接続済みを保証するため、
//! 呼び出し側の遅延接続(`RefCell`)が不要になる。

use std::error::Error;
use std::future::Future;
use std::time::{Duration, Instant};

use firestore::{FirestoreDb, FirestoreDbOptions};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use crate::error::FirelangError;

pub(super) struct EmulatorClient {
    pub(super) db: FirestoreDb,
    pub(super) runtime: Runtime,
    pub(super) project_id: String,
    /// `None` means that the runtime explicitly disabled the per-operation
    /// deadline. The constructor chooses the initial value and `Runtime` may
    /// replace it when it applies `RuntimeLimits::backend_timeout`.
    pub(super) operation_timeout: Option<Duration>,
}

pub(super) fn connect(
    host: &str,
    project_id: &str,
    operation_timeout: Option<Duration>,
) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    let db = run_operation(
        &runtime,
        operation_timeout,
        "connect",
        FirestoreDb::with_options_token_source(
            options,
            gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
            emulator_token_source(),
        ),
    )?;
    Ok(EmulatorClient {
        db,
        runtime,
        project_id: project_id.to_string(),
        // The constructor receives the same deadline used for connection
        // setup, so the first RPC and all later RPCs share one policy.
        operation_timeout,
    })
}

/// Run one synchronous SDK operation with the adapter's optional deadline.
///
/// `tokio::time::timeout` drops the pending future on expiry.  That is
/// important for streaming RPCs as well as unary calls: dropping the stream
/// cancels the request instead of allowing the response to accumulate in an
/// unbounded local vector.  The current-thread runtime remains reusable after
/// the cancellation.
pub(super) fn run_operation<T, E, F>(
    runtime: &Runtime,
    timeout: Option<Duration>,
    operation: &str,
    future: F,
) -> Result<T, Box<dyn Error>>
where
    T: Sized,
    E: Error + 'static,
    F: Future<Output = Result<T, E>>,
{
    let Some(timeout) = timeout else {
        return runtime.block_on(future).map_err(Into::into);
    };

    let started = Instant::now();
    match runtime.block_on(async move { tokio::time::timeout(timeout, future).await }) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err(timeout_error(operation, started.elapsed(), timeout)),
    }
}

fn timeout_error(operation: &str, elapsed: Duration, timeout: Duration) -> Box<dyn Error> {
    FirelangError::backend(format!(
        "Firestore emulator operation '{operation}' timed out: elapsed {}ms exceeded timeout {}ms",
        elapsed.as_millis(),
        timeout.as_millis(),
    ))
    .into()
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
    use std::future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::error::ErrorKind;

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

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

    #[test]
    fn operation_timeout_cancels_pending_future_and_runtime_can_be_reused() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let dropped = Arc::new(AtomicBool::new(false));
        let error = run_operation(&runtime, Some(Duration::from_millis(5)), "query", async {
            let _guard = DropSignal(Arc::clone(&dropped));
            future::pending::<Result<(), gcloud_sdk::tonic::Status>>().await
        })
        .expect_err("pending operation should time out");

        let firelang_error = error
            .downcast_ref::<FirelangError>()
            .expect("timeout should be a typed backend error");
        assert_eq!(firelang_error.kind, ErrorKind::Backend);
        let message = firelang_error.to_string();
        assert!(message.contains("query"), "{message}");
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("elapsed"), "{message}");
        assert!(message.contains("5ms"), "{message}");
        assert!(dropped.load(Ordering::SeqCst), "future was not cancelled");

        // A cancelled operation must not poison the reusable current-thread
        // runtime used by the adapter for the next RPC.
        let value = runtime.block_on(async {
            tokio::task::yield_now().await;
            7
        });
        assert_eq!(value, 7);
    }

    #[test]
    fn operation_timeout_none_keeps_ready_operations_working() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let value = run_operation(&runtime, None, "get", async {
            Ok::<_, gcloud_sdk::tonic::Status>(7)
        })
        .expect("ready operation");
        assert_eq!(value, 7);
    }
}
