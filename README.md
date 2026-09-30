# firelang

Firestore を操作するための **mruby 風スクリプト言語ランタイム**(Rust 実装)です。

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"
```

ローカル Firebase Auth Emulator に対する MVP の認証 API も提供します(`auth_sign_up` / `auth_sign_in` / `auth_current_user` / `auth_sign_out`)。

数値は整数のみ、変数は再代入不可、`batch` / `transaction` は原子的に適用 — といった言語・API の正式な契約はすべて **[言語仕様 / API リファレンス](docs/spec.md)** にまとまっています(このリポジトリの唯一の正式仕様です)。

## ドキュメント

- [言語仕様 / API リファレンス](docs/spec.md) — 言語・Firestore API・batch/transaction セマンティクス・Auth API の正式仕様
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

Auth エミュレータに接続する場合(`FIREBASE_AUTH_EMULATOR_HOST` が必須):

```bash
export FIREBASE_AUTH_EMULATOR_HOST=127.0.0.1:9099
export FIREBASE_PROJECT_ID=firelang-test
cargo run --bin fl -- --auth-backend emulator path/to/script.fl
```

`--backend` と `--auth-backend` は独立で、両方を `emulator` にすると両方のエミュレータへ同時に接続します。**トークンは `inspect` / `puts` / `fl` の出力には現れません**(`AuthSession(user={...}, id_token=<redacted>, refresh_token=<redacted>, expires_at=...)`)。

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start` など)。リポジトリ直下の `firebase.json` を使ってワンショットで試す例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'

firebase emulators:exec --only auth --project demo-firelang \
  'FIREBASE_PROJECT_ID=demo-firelang cargo test --test auth_emulator -- --nocapture'
```

本番 Firestore / 本番 Firebase Authentication への接続は非対応です。詳細は [仕様の 12-13 章](docs/spec.md#12-バックエンド) を参照してください。

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
cargo test --test auth              # Auth の in-memory contract テスト
```

エミュレータ統合テスト(`firestore_emulator` / `auth_emulator`)は対応する emulator host 変数が未設定なら早期 return でスキップされます。実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --test firestore_emulator -- --nocapture
cargo test --test auth_emulator -- --nocapture
```

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo build --release --bin fl`
- **`integration-emulator` ジョブ**: Firestore / Auth エミュレータを起動してそれぞれの統合テストを実行
- **`CD`**: `v*` タグ push で `fl` の Linux バイナリをビルドして GitHub Release に添付

## ライセンス

[MIT](LICENSE)
