# firelang

Firestore と Cloud Storage を操作するための **mruby 風スクリプト言語ランタイム**(Rust 実装)です。

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"

payload = bytes([0, 255, 128])
storage_upload("avatars/alice.bin", payload, content_type: "application/octet-stream")
content = storage_download("avatars/alice.bin")
```

数値は整数のみ、変数は再代入不可、`batch` / `transaction` は原子的に適用 — といった言語・API の正式な契約はすべて **[言語仕様 / API リファレンス](docs/spec.md)** にまとまっています(このリポジトリの唯一の正式仕様です)。

## ドキュメント

- [言語仕様 / API リファレンス](docs/spec.md) — 言語・Firestore / Storage API・batch/transaction セマンティクスの正式仕様
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

本番 Firestore および本番 Cloud Storage(認証付き)は非対応です。詳細は [仕様のバックエンド](docs/spec.md#11-バックエンド) と [Cloud Storage API](docs/spec.md#14-cloud-storage-api) を参照してください。

## Cloud Storage

Storage の MVP CRUD は `storage_upload` / `storage_download` / `storage_metadata` / `storage_delete` のグローバル関数です。`bytes([0, 255])` は UTF-8 文字列ではなくバイト列で、ダウンロード時も text 化されません。Storage object path は Firestore の document path と異なり、セグメント数の偶奇を要求しません。

`firebase.json` にはローカル用の `storage.rules` と port 9199 の emulator を設定しています。Storage だけを検証する quickstart:

```bash
export FIREBASE_PROJECT_ID=demo-firelang
firebase emulators:exec --only storage --project demo-firelang \
  'cargo test --test storage_emulator -- --nocapture'
```

Firestore と Storage を同時に使う場合は `--only firestore,storage` を指定します。`fl` は `--storage-backend emulator` で `FIREBASE_STORAGE_EMULATOR_HOST` を明示的に要求でき、未指定の `auto` では host があれば Storage emulator、なければ in-memory を使います。`FIREBASE_PROJECT_ID`（未設定時は `GCLOUD_PROJECT` / `FIRESTORE_PROJECT_ID` / `firelang-test`）からバケットを導出します。必要なら `FIREBASE_STORAGE_BUCKET` と、local rules テスト用の明示的な `FIREBASE_STORAGE_AUTH_TOKEN` も設定できます。本番 token の自動取得は MVP の対象外です。

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
```

エミュレータ統合テストは `FIRESTORE_EMULATOR_HOST` が未設定なら早期 return でスキップされます。実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --test firestore_emulator -- --nocapture
cargo test --test storage_emulator -- --nocapture       # FIREBASE_STORAGE_EMULATOR_HOST 未設定なら skip
```

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo test --all-targets`, `cargo build --release --bin fl`
- **`integration-emulator` ジョブ**: Firestore エミュレータを起動して `cargo test --test firestore_emulator` を実行
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## ライセンス

[MIT](LICENSE)
