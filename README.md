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
- [Firebase rules の分離とデプロイ防止策](docs/firebase-security.md)
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

エミュレータは Firebase CLI で起動します。
Local emulator には allow-all rules を持つ `firebase.emulators.json` を明示し、project ID には `demo-firelang` を指定します。

```bash
firebase emulators:start --config firebase.emulators.json --only firestore --project demo-firelang
```

ワンショットで統合テストを実行する例:

```bash
firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'
```

`firebase.json` は deploy 用であり、Firestore と Storage の read/write を拒否します。
`firebase deploy --config firebase.emulators.json` は実行しないでください。
詳細は [Firebase rules の分離とデプロイ防止策](docs/firebase-security.md) を参照してください。

本番 Firestore（認証付き）と Cloud Storage は非対応です。
詳細は [仕様の 11-12 章](docs/spec.md#11-バックエンド) を参照してください。

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
cargo test --test firebase_config   # Firebase deploy guard
```

エミュレータ統合テストは `FIRESTORE_EMULATOR_HOST` が未設定なら早期 return でスキップされます。実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --test firestore_emulator -- --nocapture
```

## CI / CD (GitHub Actions)

- **`test` ジョブ**: format、Clippy、全 test、Firebase deployment guard、release build を実行
- **`integration-emulator` ジョブ**: allow-all の local config で Firestore テストを実行し、deploy config が write を拒否することも確認
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## ライセンス

[MIT](LICENSE)
