# firelang

Firestore を操作するための **mruby 風スクリプト言語ランタイム**(Rust 実装)です。

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"
```

数値は整数のみ、変数は再代入不可、`batch` / `transaction` は原子的に適用 — といった言語・API の正式な契約はすべて **[言語仕様 / API リファレンス](docs/spec.md)** にまとまっています(このリポジトリの唯一の正式仕様です)。

## ドキュメント

- [言語仕様 / API リファレンス](docs/spec.md) — 言語・Firestore API・batch/transaction セマンティクスの正式仕様
- [Services 構成の設計](docs/services.md) — Firestore / Auth / Storage の合成、互換性 adapter、capability error、同期 client の制約
- [examples/scripts/](examples/scripts/) — 動くサンプルスクリプト集
- [リリースチェックリスト](docs/release-checklist.md)
- [過去の MVP 計画資料(アーカイブ)](docs/archive/mvp/)

## 使い方

```bash
cargo run --bin fl -- path/to/script.fl
```

デフォルトは in-memory バックエンドです。Firestore エミュレータに接続する場合(`FIRESTORE_EMULATOR_HOST` が必須。プロジェクト ID は省略時 `firelang-test`):

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo run --bin fl -- --backend emulator path/to/script.fl
```

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start --only firestore` など)。リポジトリ直下の `firebase.json` を使ってワンショットで試す例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'
```

本番 Firestore(認証付き)は非対応です。詳細は [仕様の 11-12 章](docs/spec.md#11-バックエンド) を参照してください。

## Services の組み合わせ

`fl` の in-memory backend と `run_script` は、Firestore、Auth、Storage の in-memory 実装をすべて設定します。Rust から Firestore emulator を `Services` へ渡す場合も、Auth と Storage は個別に差し替えられます。

```ruby
user = auth_sign_in_anonymously()
storage_put("reports/today.txt", "done", content_type: "text/plain")
puts "signed in as #{user["uid"]}"
```

```rust
use firelang::{InMemoryFirestore, InMemoryStorage, Runtime, RuntimeContext, Services};

let services = Services::builder()
    .firestore(InMemoryFirestore::default())
    .storage(InMemoryStorage::default())
    .build();
let mut runtime: Runtime = Runtime::from_context(RuntimeContext::new(services));
```

`run_script_with_firestore` は従来の Firestore だけを受け取る adapter です。Auth や Storage を明示的に設定していないため、それらを呼び出すと `capability unavailable` エラーになります。設計上の判断は [Services 構成の設計](docs/services.md) にまとめています。

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
```

エミュレータ統合テストは `FIRESTORE_EMULATOR_HOST` が未設定なら早期 return でスキップされます。実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --test firestore_emulator -- --nocapture
```

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo test --all-targets`, `cargo build --release --bin fl`
- **`integration-emulator` ジョブ**: Firestore エミュレータを起動して `cargo test --test firestore_emulator` を実行
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## ライセンス

[MIT](LICENSE)
