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
) -> Result<EmulatorClient, Box<dyn Error>> {
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("create tokio runtime for Firestore SDK");
    let options = sdk_options(host, project_id);
    let db = runtime.block_on(FirestoreDb::with_options_token_source(
        options,
        gcloud_sdk::GCP_DEFAULT_SCOPES.clone(),
        emulator_token_source(auth),
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
