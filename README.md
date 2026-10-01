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

ローカル Firebase Auth Emulator に対する MVP の認証 API も提供します(`auth_sign_up` / `auth_sign_in` / `auth_current_user` / `auth_sign_out`)。

数値は整数のみ、変数は再代入不可、`batch` / `transaction` は原子的に適用 — といった言語・API の正式な契約はすべて **[言語仕様 / API リファレンス](docs/spec.md)** にまとまっています(このリポジトリの唯一の正式仕様です)。

## ドキュメント

- [言語仕様 / API リファレンス](docs/spec.md) — 言語・Firestore / Storage API・batch/transaction セマンティクス・Auth API の正式仕様
- [examples/scripts/](examples/scripts/) — 動くサンプルスクリプト集
- [Firebase rules の分離とデプロイ防止策](docs/firebase-security.md)
- [リリースチェックリスト](docs/release-checklist.md)
- [過去の MVP 計画資料(アーカイブ)](docs/archive/mvp/)

## 使い方

```bash
# 既定ビルド(in-memory のみ。emulator SDK はリンクされない)
cargo run --bin fl -- path/to/script.fl
```

Firestore エミュレータバックエンドは `emulator` feature を明示して有効にします(`FIRESTORE_EMULATOR_HOST` が必須。プロジェクト ID は省略時 `firelang-test`):

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo run --features emulator --bin fl -- --backend emulator path/to/script.fl
```

エミュレータは Firebase CLI で起動します。
Local emulator には allow-all rules を持つ `firebase.emulators.json` を明示し、project ID には `demo-firelang` を指定します。

```bash
firebase emulators:start --config firebase.emulators.json --only firestore --project demo-firelang
```

ワンショットで統合テストを実行する例:

```bash
firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator --locked -- --nocapture'
```

`firebase.json` は deploy 用であり、Firestore と Storage の read/write を拒否します。
`firebase deploy --config firebase.emulators.json` は実行しないでください。
詳細は [Firebase rules の分離とデプロイ防止策](docs/firebase-security.md) を参照してください。

`fl --backend emulator` は feature なしで実行すると、SDK を読み込まずに
`emulator` feature の有効化方法を案内して終了します。in-memory 専用の
consumer は `cargo build --no-default-features` でも同じ in-memory API を利用できます。

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start --only firestore` など)。リポジトリ直下の `firebase.json` を使ってワンショットで試す例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator -- --nocapture'
```

本番 Firestore および本番 Cloud Storage(認証付き)は非対応です。詳細は [仕様のバックエンド](docs/spec.md#12-バックエンド) と [Cloud Storage API](docs/spec.md#15-cloud-storage-api) を参照してください。

## Cloud Storage

Storage の MVP CRUD は `storage_upload` / `storage_download` / `storage_metadata` / `storage_delete` のグローバル関数です。`bytes([0, 255])` は UTF-8 文字列ではなくバイト列で、ダウンロード時も text 化されません。Storage object path は Firestore の document path と異なり、セグメント数の偶奇を要求しません。

`firebase.json` にはローカル用の `storage.rules` と port 9199 の emulator を設定しています。Storage だけを検証する quickstart:

```bash
export FIREBASE_PROJECT_ID=demo-firelang
firebase emulators:exec --only storage --project demo-firelang \
  'cargo test --test storage_emulator -- --nocapture'
```

Firestore と Storage を同時に使う場合は `--only firestore,storage` を指定します。`fl` は `--storage-backend emulator` で `FIREBASE_STORAGE_EMULATOR_HOST` を明示的に要求でき、未指定の `auto` では host があれば Storage emulator、なければ in-memory を使います。`FIREBASE_PROJECT_ID`（未設定時は `GCLOUD_PROJECT` / `FIRESTORE_PROJECT_ID` / `firelang-test`）からバケットを導出します。必要なら `FIREBASE_STORAGE_BUCKET` と、local rules テスト用の明示的な `FIREBASE_STORAGE_AUTH_TOKEN` も設定できます。本番 token の自動取得は MVP の対象外です。
Auth エミュレータに接続する場合(`FIREBASE_AUTH_EMULATOR_HOST` が必須):

```bash
export FIREBASE_AUTH_EMULATOR_HOST=127.0.0.1:9099
export FIREBASE_PROJECT_ID=firelang-test
cargo run --features emulator --bin fl -- --auth-backend emulator path/to/script.fl
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
cargo test --all-targets            # ユニット + 統合テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
cargo test --test release           # release tag/package の検証
cargo test --test auth              # Auth の in-memory contract テスト
cargo test --test firebase_config   # Firebase deploy guard

# 既定 feature なし: in-memory のみ
cargo check --no-default-features --all-targets
cargo test --no-default-features --all-targets

# emulator SDK を有効にしたビルド
cargo check --features emulator --all-targets
cargo test --features emulator --all-targets
```

エミュレータ統合テスト(`firestore_emulator` / `storage_emulator` / `auth_emulator`)は対応する emulator host 変数が未設定なら早期 return でスキップされます。`emulator` feature を有効にして、実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --features emulator --test firestore_emulator -- --nocapture
cargo test --features emulator --test storage_emulator -- --nocapture   # FIREBASE_STORAGE_EMULATOR_HOST 未設定なら skip
cargo test --features emulator --test auth_emulator -- --nocapture      # FIREBASE_AUTH_EMULATOR_HOST 未設定なら skip
```

`Firestore` / `InMemoryFirestore` / `run_script` は feature に依存せず利用できます。
`FirestoreEmulator` と `fl --backend emulator` は `emulator` feature でのみ公開・有効です。
ライブラリとして emulator backend を使う consumer は、依存宣言に
`features = ["emulator"]` を指定します。

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo build --release --bin fl` を実行
- **`feature-builds` ジョブ**: default / `--no-default-features` / `--features emulator` のビルド境界を検証
- **`integration-emulator` ジョブ**: Firestore / Storage / Auth エミュレータを起動して各統合テストを実行
- CI は reusable workflow になっており、`v*` タグを押した CD も同じジョブ群（integration test を含む）を必ず実行します。CD の release ジョブはこの gate と tag version check に依存するため、branch CI を飛ばした tag でも公開できません。
- **CD**: tag の `v` を除いた部分（例: `v0.1.0` → `0.1.0`）が `cargo metadata --no-deps` の `firelang` package version と一致する場合だけ、`fl` の Linux バイナリを `--features emulator` でビルドして release します。
- release asset は raw binary ではなく `firelang-<version>-linux-x86_64.tar.gz` と SHA-256 `.sha256` sidecar です。tarball には実行権限付きの `fl`、`README.md`、`LICENSE`、`docs/spec.md`、ファイル単位の `SHA256SUMS` が含まれます。

ダウンロード後は、archive とその sidecar を同じディレクトリに置いて次のように検証できます。

```bash
sha256sum -c firelang-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf firelang-0.1.0-linux-x86_64.tar.gz
(cd firelang-0.1.0-linux-x86_64 && sha256sum -c SHA256SUMS)
```

- CI の deploy 用 `firebase.json` が write を拒否することも `integration-emulator` ジョブで確認します。


## ライセンス

[MIT](LICENSE)
