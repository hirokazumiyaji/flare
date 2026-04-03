# firelang

Firestore を操作するための **mruby 風スクリプト言語ランタイム** (Rust 実装) です。

## MVP の方針

現時点は mruby 完全互換ではなく、Firestore 向けの軽量サブセットです。

- 変数代入 (`a = 1`)
- 四則演算・比較演算
- `fn / if / while / return`
- `{ ... }` ブロック
- 文字列埋め込み `"hello #{name} #{1 + 2}"`
- 組み込み `puts`
- Ruby 風 keyword argument (`merge: true`, `fn(a: 1, b: 2)`)
- 変数は再代入不可（immutable）
- object method は変更後の copy を返す（元オブジェクトは不変）

## Firestore 向け API（global function）

`firestore` オブジェクトは使わず、以下をグローバル関数で提供します。

- `doc("users/1")`
- `collection("users")`
- `collection_group("members")`
- `get(doc_or_path)`
- `set(doc_or_path, data_map, merge_bool = false)`
- `update(doc_or_path, data_map)`
- `delete(doc_or_path)`
- `transaction { ... }`
- `batch { ... }`

`set` / `update` の `data_map` は JavaScript object 風の記法で指定します。

```ruby
set(doc("users/1"), {name: "alice", age: 20})
set("users/1", {age: 21}, true) # merge
```

## Query API

- `collection("users").where("age", ">=", 20).limit(10).get()`
- `collection_group("members").where("active", "==", true).get()`
- `collection("users").doc("1")` は `doc("users/1")` 相当

## Timestamp / Date 系

- `now()`
- `timestamp(1700000000)`
- `timestamp("2026-02-01T00:00:00+09:00")`
- `ts.to_unix()`
- `ts.add_seconds(3600)`

## Firestore 変換系 helper

- `[1, 2, ""]` 形式の配列リテラル
- `array(...)`
- `array_union(array(...))`
- `array_remove(array(...))`
- `server_timestamp()`

## 使い方

```bash
cargo run --bin fl -- path/to/script.fl
```

## テスト

```bash
cargo test
```

example script のみをまとめて実行:

```bash
cargo test --test example_scripts
```

Emulator テスト:

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo test firestore_global_functions_with_emulator -- --nocapture
```

## CI / CD (GitHub Actions)

- `CI`: push / pull_request ごとに `cargo fmt --check`, `cargo test --all-targets`, `cargo build --release --bin fl` を実行。
- `CD`: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付。

## Firestore 依存について

- 現状 MVP は Firestore 公式 Rust SDK には依存していません。
- ランタイムは `Firestore` trait で抽象化し、デフォルトは `InMemoryFirestore` を使います。
- Emulator テストは REST API を `curl` で叩く軽量アダプタです。
- 本番 Firestore 接続には認証付き HTTP クライアント等の追加実装が必要です。
