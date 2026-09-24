//! テスト共有ヘルパー。各統合テストから `#[macro_use] mod common;` で使う。

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
