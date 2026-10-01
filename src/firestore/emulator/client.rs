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

/// Firestore emulator に対して使う資格情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmulatorAuth {
    /// `Bearer owner`。emulator の admin 資格で、security rules を
    /// 完全にバイパスする。通常の開発・テストではこちらを使う。
    Owner,
    /// 権限のない(未認証の)資格情報。emulator が security rules を
    /// 評価するようになるので、rules が deny している場合は
    /// `PERMISSION_DENIED` が返る。rules の検証に使わなければならない。
    EnforceRules,
}

pub(super) fn connect(
    host: &str,
    project_id: &str,
    auth: EmulatorAuth,
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
            emulator_token_source(auth),
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

/// `alg: none` の未署名 JWT。header `{"alg":"none"}` / payload
/// `{"sub":"unauthenticated"}` を base64url で並べたもの。
///
/// emulator 側で「有効な JWT 形式だが admin ではない」資格として扱われ、
/// security rules を評価させる。`owner` は admin で rules をバイパスし、
/// `unauthenticated` のような素の文字列は `invalid jwt` で 400 になるため、
/// rules 検証にはこの形式が必要になる(実測で 403 PERMISSION_DENIED を確認)。
const NON_ADMIN_JWT: &str = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1bmF1dGhvcmljYXRlZCJ9.";

fn emulator_token_source(auth: EmulatorAuth) -> gcloud_sdk::TokenSourceType {
    // `owner` は admin 資格で rules をバイパスする。rules を検証したい
    // ときは admin でない token を送る必要がある。
    let token = match auth {
        EmulatorAuth::Owner => "owner",
        EmulatorAuth::EnforceRules => NON_ADMIN_JWT,
    };
    gcloud_sdk::TokenSourceType::ExternalSource(Box::new(
        gcloud_sdk::ExternalJwtFunctionSource::new(move || {
            std::future::ready(Ok(gcloud_sdk::Token::new(
                "Bearer".to_string(),
                token.into(),
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

}
