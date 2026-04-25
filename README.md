# firelang

Firestore を操作するための **mruby 風スクリプト言語ランタイム** (Rust 実装) です。

## MVP Contract

This section is the authoritative MVP contract. Planning notes live under `docs/mvp/`.

### Backends

| Entrypoint | `in-memory` | `emulator` |
|------------|-------------|------------|
| `run_script` | Yes (default) | No |
| `run_script_with_firestore` | Yes | Yes, when passed an emulator backend |
| `fl` CLI | Yes (default) | Yes, with `--backend emulator` |

Production Firestore with authentication is **out of scope** for MVP.

### Types and values

- Numeric values are **integers only** (`Int`). Float support is post-MVP.
- Query result ordering is deterministic: sort by the **firelang relative document path string** in lexicographic order (not by bare document id). For example, `users/a` sorts before `users/b`, and `teams/t1/members/a` sorts before `teams/t2/members/a`.
- Documented `limit` on queries is in scope. Cursor-based pagination and offset-based pagination beyond that are **out of scope**.

### Reads and writes

- `update` on a missing document **fails** and does **not** create the document.

### `batch`

- `batch` is **write-only**: it stages `set`, `update`, and `delete`.
- On success, all staged writes are applied atomically. On **runtime error** or **backend error**, **no** writes are persisted (no partial writes).

### `transaction`

- Allowed: `get`, `set`, `update`, `delete`.
- Multiple `get` calls are allowed **before** the first staged write (including repeated reads of the same document and reads of different documents).
- After the first staged write, **`get` is not allowed** and fails with a clear error.
- Queries inside `transaction { ... }` are **out of scope** for MVP.
- Single attempt only: no automatic retries.
- On runtime or backend failure, **no** writes from that transaction are persisted.

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

エミュレータに接続する場合（`FIRESTORE_EMULATOR_HOST` が必須。プロジェクト ID は REST の URL に使われます）:

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo run --bin fl -- --backend emulator path/to/script.fl
```

**Firebase CLI でエミュレータを起動してから**（別ターミナルで `firebase emulators:start --only firestore` など）、上記の環境変数を設定して `fl` を実行するか、リポジトリ直下の `firebase.json` を使ってワンショットでテストする例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'
```

（CI の `integration-emulator` ジョブと同じ流れです。）

## テスト

```bash
cargo test
```

example script のみをまとめて実行:

```bash
cargo test --test example_scripts
```

CLI のスモークテスト（`fl` の `--help` と不正 `--backend`）:

```bash
cargo test --test cli
```

Emulator 統合テスト（`FIRESTORE_EMULATOR_HOST` が無い環境では早期 return でパスするだけです。実際に叩くには上記 `firebase emulators:exec` か、手動でエミュレータ起動後に環境変数を設定）:

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo test --test firestore_emulator -- --nocapture
```

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo test --all-targets`, `cargo build --release --bin fl`
- **`integration-emulator` ジョブ**: Node + `firebase-tools` + Java で Firestore エミュレータを起動し、`cargo test --test firestore_emulator` を実行（上記 `demo-firelang` / `FIRESTORE_PROJECT_ID`）
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## リリース前チェック

[docs/release-checklist.md](docs/release-checklist.md) を参照してください。

## Firestore 依存について

- 現状 MVP は Firestore 公式 Rust SDK には依存していません。
- ランタイムは `Firestore` trait で抽象化し、デフォルトは `InMemoryFirestore` を使います。
- エミュレータ向け実装は Firestore REST を `ureq` で呼び出します（`src/emulator.rs`）。
- 本番 Firestore 接続には認証付き HTTP クライアント等の追加実装が必要です。
