//! テスト共有ヘルパー。各 integration test から `mod common;` で使う。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
static NEXT_UNIQUE: AtomicU64 = AtomicU64::new(0);

/// 同じプロセス内でも衝突しない一意な ID を返す。process ID と時刻も組み合わせるため、
/// integration test binary を分けて実行しても同じ永続 emulator 上で衝突しない。
#[allow(dead_code)]
pub(crate) fn unique(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    let sequence = NEXT_UNIQUE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{nanos}_{sequence}", std::process::id())
}

/// test 固有の ancestor document。配下の collection を別の test と共有しない。
#[allow(dead_code)]
pub(crate) fn test_namespace(test_name: &str) -> String {
    format!("_flare_tests/{test_name}_{}", unique("run"))
}

/// test 固有の subcollection。通常の emulator integration test fixture はここへ書く。
#[allow(dead_code)]
pub(crate) fn test_collection(test_name: &str) -> String {
    format!("{}/docs", test_namespace(test_name))
}

/// `Value::Map` リテラル。キーは文字列式なら何でもよい(`&str` を想定)。
#[macro_export]
macro_rules! map {
    ($($k:expr => $v:expr),* $(,)?) => {
        Value::Map(
            [$( ( ($k).to_string(), ($v)) ),*]
                .into_iter()
                .collect::<std::collections::BTreeMap<String, Value>>(),
        )
    };
}
