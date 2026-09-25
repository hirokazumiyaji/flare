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

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start --only firestore` など)。リポジトリ直下の `firebase.json` を使って、同じ emulator に対して library / CLI の integration test を 2 回実行する例:

```bash
FIRESTORE_PROJECT_ID=demo-firelang firebase emulators:exec \
  --only firestore --project demo-firelang \
  'set -eu
    env | grep -q "^FIRESTORE_EMULATOR_HOST="
    for run in 1 2; do
      cargo test --test firestore_emulator --test cli -- --nocapture
    done'
```

本番 Firestore(認証付き)は非対応です。詳細は [仕様の 11-12 章](docs/spec.md#11-バックエンド) を参照してください。

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
```

エミュレータ統合テストは `FIRESTORE_EMULATOR_HOST` が未設定なら早期 return でスキップされます。永続 emulator へ接続中は、CI と同じ library / CLI test matrix を実行でき、同じコマンドの繰り返しや別 test process 同士でも contention しません:

```bash
FIRESTORE_PROJECT_ID=demo-firelang \
  cargo test --test firestore_emulator --test cli -- --nocapture
```

各 test は `_firelang_tests/<test-name>_<unique-run>/...` の固有 hierarchy へ書き込みます。環境変数が設定済みで接続に失敗した場合は skip せず失敗します。

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo test --all-targets`, `cargo build --release --bin fl`
- **`integration-emulator` ジョブ**: Firestore エミュレータを起動して library / CLI の integration test を同じ emulator 上で 2 回実行
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## ライセンス

[MIT](LICENSE)
