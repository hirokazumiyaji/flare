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
    pub(super) operation_timeout: Duration,
}

pub(super) fn connect(
    host: &str,
    project_id: &str,
    operation_timeout: Duration,
) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    // The SDK has its own (30 second) connect timeout.  Keep the application
    // timeout as the outer guard as well, so an embedding caller is not forced
    // to wait for the SDK's transport default.
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
        operation_timeout,
    })
}

/// Run one synchronous SDK operation on the adapter's current-thread runtime.
///
/// `tokio::time::timeout` owns the RPC future while it is pending.  On expiry,
/// the future is dropped, which cancels the request/stream before the runtime
/// is returned to the caller.  The runtime is intentionally kept on the
/// client and reused for the next operation.
pub(super) fn run_operation<T, E, F>(
    runtime: &Runtime,
    timeout: Duration,
    operation: &str,
    future: F,
) -> Result<T, Box<dyn Error>>
where
    T: Sized,
    E: Error + 'static,
    F: Future<Output = Result<T, E>>,
{
    let started = Instant::now();
    match runtime.block_on(async move { tokio::time::timeout(timeout, future).await }) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Err(timeout_error(operation, started.elapsed(), timeout)),
    }
}

fn timeout_error(operation: &str, elapsed: Duration, timeout: Duration) -> Box<dyn Error> {
    // Keep both values in the message: the observed elapsed time is useful
    // when diagnosing a slow machine, while the limit explains why the RPC
    // was cancelled.  `as_millis` is deliberately used instead of Debug's
    // duration representation so CLI/API errors are stable and easy to grep.
    FirelangError::backend(format!(
        "Firestore emulator operation '{operation}' timed out: elapsed {}ms exceeded timeout {}ms",
        elapsed.as_millis(),
        timeout.as_millis(),
    ))
    .into()
}

/// このエラーが「操作の成否が不明な timeout」由来かどうか。
///
/// timeout は future を drop するので、server 側が commit を適用したかを
/// 知ることができない。決定的な失敗と区別することで、呼び出し側が Rollback
/// で状態を確定させ、再試行による二重適用を避けられる。
pub(super) fn is_timeout_error(error: &dyn Error) -> bool {
    error.to_string().contains("timed out")
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
    fn operation_timeout_cancels_future_and_runtime_can_be_reused() {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let dropped = Arc::new(AtomicBool::new(false));
        let result = run_operation(&runtime, Duration::from_millis(5), "query", async {
            let _guard = DropSignal(Arc::clone(&dropped));
            future::pending::<Result<(), gcloud_sdk::tonic::Status>>().await
        });

        let error = result.expect_err("pending operation should time out");
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

        // A timeout must not poison the runtime.  This is deliberately a
        // second block_on call, matching the adapter's operation model.
        let value = runtime.block_on(async {
            tokio::task::yield_now().await;
            7
        });
        assert_eq!(value, 7);
    }

    #[test]
    fn timeout_errors_are_distinguishable_from_definitive_failures() {
        // `commit_transaction` は timeout(結果不明)Rollback で状態を確定
        // させるので、timeout を決定的な失敗と区別できる必要がある。
        let timed_out: Box<dyn Error> = FirelangError::backend(
            "Firestore emulator operation 'transaction_commit' timed out: elapsed 10ms exceeded timeout 5ms",
        )
        .into();
        assert!(is_timeout_error(timed_out.as_ref()));

        let definitive: Box<dyn Error> =
            FirelangError::backend("Firestore emulator operation 'transaction_commit' failed")
                .into();
        assert!(!is_timeout_error(definitive.as_ref()));
    }
}
