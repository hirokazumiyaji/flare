//! SDK 接続・トークン・runtime。`EmulatorClient` は接続済みを保証するため、
//! 呼び出し側の遅延接続(`RefCell`)が不要になる。

use std::error::Error;
use std::future::Future;
use std::time::{Duration, Instant};

use firestore::{FirestoreDb, FirestoreDbOptions};
use gcloud_sdk::google::firestore::v1::GetDocumentRequest;
use gcloud_sdk::tonic::Request;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use super::path;
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

/// Collection used only for the connect-time liveness probe.  It is never
/// written to; the probe is a read of a document that does not exist.
const PROBE_COLLECTION: &str = "_firelang_connection_probe";

/// Fail-fast deadline for the connect step, independent of the caller's
/// per-operation timeout.  A host that accepts a TCP connection but never
/// completes the HTTP/2 handshake would otherwise block the caller for the
/// whole operation timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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
    operation_timeout: Option<Duration>,
) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    // The SDK has its own (30 second) connect timeout.  Keep the connect step
    // bounded by the shorter of the caller-supplied operation timeout and the
    // dedicated connect deadline so an embedding caller is not forced to wait
    // for the SDK's transport default.
    //
    // A caller that disabled the *operation* deadline (`RuntimeLimits`
    // unlimited) still gets the connect deadline: disabling per-operation
    // budgets must not turn a dead host into an unbounded constructor call.
    let connect_timeout = match operation_timeout {
        Some(limit) => limit.min(CONNECT_TIMEOUT),
        None => CONNECT_TIMEOUT,
    };
    // One deadline for the whole connection sequence.  Client creation and the
    // probe run back to back, so a separate full timeout for each would let a
    // constructor configured for 5 seconds block for nearly 10.
    let sequence_started = Instant::now();
    let db = run_operation(
        &runtime,
        Some(connect_timeout),
        "connect",
        FirestoreDb::with_options_token_source(
            options,
            gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
            emulator_token_source(auth),
        ),
    )?;
    let probe_timeout = remaining_budget(connect_timeout, sequence_started.elapsed());

    // The SDK's channel can be created after the TCP socket is accepted even
    // when the peer never completes the HTTP/2 handshake.  Probe one harmless
    // read so `connect` has a real fail-fast deadline instead of returning a
    // client that only fails on the user's first Firestore operation.
    //
    // The probe only proves that the peer speaks Firestore, so an answer that
    // says "the request was refused" is still a reachable emulator: NOT_FOUND
    // (the probe document does not exist) and PERMISSION_DENIED (security
    // rules deny every read, which is exactly how the deploy-config deny rules
    // are verified) both mean the request reached Firestore.  Any other status
    // means something else is listening on this port, or that the credentials
    // the caller asked for were rejected, and either way `connect` must fail
    // instead of handing back a client that only fails on the first write.
    // The probe path must be a legal Firestore document path: the emulator
    // rejects reserved collection ids (`__name__`, `__reserved__`) with
    // INVALID_ARGUMENT, which is a configuration error rather than a
    // successful "no such document".
    let probe_name = path::document_name(project_id, &format!("{}/probe", PROBE_COLLECTION));
    let probe = {
        let db = &db;
        async move {
            let mut service = db.client().get();
            service
                .get_document(Request::new(GetDocumentRequest {
                    name: probe_name,
                    mask: None,
                    consistency_selector: None,
                }))
                .await
        }
    };
    match runtime.block_on(async move { tokio::time::timeout(probe_timeout, probe).await }) {
        // `Err(_)` is the deadline firing; the status itself is reported below.
        Err(_) => {
            return Err(FirelangError::backend(format!(
                "Firestore emulator connection probe timed out after {}ms (connect budget {}ms)",
                probe_timeout.as_millis(),
                connect_timeout.as_millis()
            ))
            .into())
        }
        Ok(Ok(_)) => {}
        // A denied read is expected for the EnforceRules/deny-rules setup, and
        // the emulator also answers UNAUTHENTICATED when the owner token is
        // refused (the rules-deployed project rejects the owner's requests
        // with exactly that status).  All three statuses prove the peer
        // understood a Firestore request, which is what the probe is for.
        Ok(Err(status)) if is_reachable_probe_answer(status.code()) => {}
        Ok(Err(status)) => {
            return Err(FirelangError::backend(format!(
                "Firestore emulator connection probe failed: {status}"
            ))
            .into())
        }
    }
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
    timeout: Option<Duration>,
    operation: &str,
    future: F,
) -> Result<T, Box<dyn Error>>
where
    T: Sized,
    E: Error + 'static,
    F: Future<Output = Result<T, E>>,
{
    // A disabled deadline runs the operation to completion: nothing is dropped
    // on expiry, so a caller that opts out of the budget accepts that an
    // unresponsive backend blocks instead of being cancelled.
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

/// Budget left for the connect probe after client creation consumed part of it.
///
/// `connect` advertises one fail-fast deadline for the whole sequence, so the
/// probe may only spend what the client did not.  Saturating subtraction keeps
/// the result non-negative, which makes a stalled probe fail immediately
/// instead of being handed a second full budget.
fn remaining_budget(connect_timeout: Duration, elapsed: Duration) -> Duration {
    connect_timeout.saturating_sub(elapsed)
}

/// Statuses that still prove the peer is a Firestore emulator.
///
/// `UNAUTHENTICATED` is deliberately **not** accepted: an authentication
/// gateway in front of a non-Firestore gRPC service answers with it, and so
/// does an emulator that rejected the credentials the caller asked for.
/// Returning a client in that state would move the failure to the caller's
/// first real operation instead of reporting it at construction time.
///
/// Anything else (UNAVAILABLE, UNIMPLEMENTED, INTERNAL, ...) means the port
/// belongs to a different service or the emulator is broken, and `connect`
/// must not hand back a client that only fails on the caller's first write.
fn is_reachable_probe_answer(code: gcloud_sdk::tonic::Code) -> bool {
    matches!(
        code,
        gcloud_sdk::tonic::Code::NotFound | gcloud_sdk::tonic::Code::PermissionDenied
    )
}

fn timeout_error(operation: &str, elapsed: Duration, timeout: Duration) -> Box<dyn Error> {
    // `as_millis()` は切り捨てなので、500µs のような sub-millisecond の予算が
    // "timeout 0ms" と誤表示され、運用者が設定値を判断できなくなる。
    // `Duration` の `Debug` は設定単位に合わせて出るため、runtime 側の
    // `format_duration` と同じ表記に揃える。
    FirelangError::backend(format!(
        "Firestore emulator operation '{operation}' timed out: \
         elapsed {elapsed:?} exceeded timeout {timeout:?}"
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
            tx.send(
                connect(
                    &host,
                    "p",
                    EmulatorAuth::Owner,
                    Some(Duration::from_secs(30)),
                )
                .is_err(),
            )
            .expect("send result");
        });
        assert!(rx
            .recv_timeout(CONNECT_TIMEOUT + Duration::from_secs(2))
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

    #[test]
    fn probe_accepts_refusals_but_rejects_other_services() {
        // These two still prove the peer is a Firestore emulator: the probe
        // document is absent, or the rules deny every read. The deny-rules CI
        // check depends on the second case.
        for code in [
            gcloud_sdk::tonic::Code::NotFound,
            gcloud_sdk::tonic::Code::PermissionDenied,
        ] {
            assert!(is_reachable_probe_answer(code), "{code:?}");
        }
        // A port owned by a different service, or credentials the emulator
        // refused, must still fail `connect` instead of handing back a client
        // whose first real operation fails.
        for code in [
            gcloud_sdk::tonic::Code::Unauthenticated,
            gcloud_sdk::tonic::Code::Unavailable,
            gcloud_sdk::tonic::Code::Unimplemented,
            gcloud_sdk::tonic::Code::Internal,
            gcloud_sdk::tonic::Code::InvalidArgument,
            gcloud_sdk::tonic::Code::DeadlineExceeded,
        ] {
            assert!(!is_reachable_probe_answer(code), "{code:?}");
        }
    }

    #[test]
    fn probe_only_gets_the_budget_the_client_creation_left() {
        // Client creation plus the probe share one deadline.
        let budget = Duration::from_millis(500);
        assert_eq!(remaining_budget(budget, Duration::from_millis(0)), budget);
        assert_eq!(
            remaining_budget(budget, Duration::from_millis(200)),
            Duration::from_millis(300)
        );
        // Overrunning the budget must not wrap around into a second full
        // budget for the probe.
        assert_eq!(
            remaining_budget(budget, Duration::from_millis(900)),
            Duration::ZERO
        );
    }

    #[test]
    fn operation_timeout_cancels_future_and_runtime_can_be_reused() {
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
        // A sub-millisecond budget must not render as "0ms".
        let error = run_operation(
            &runtime,
            Some(Duration::from_micros(500)),
            "query",
            future::pending::<Result<(), gcloud_sdk::tonic::Status>>(),
        )
        .expect_err("pending operation should time out");
        assert!(error.to_string().contains("500µs"), "{error}");
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
