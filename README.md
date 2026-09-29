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
# 既定ビルド(in-memory のみ。emulator SDK はリンクされない)
cargo run --bin fl -- path/to/script.fl
```

Firestore エミュレータバックエンドは `emulator` feature を明示して有効にします(`FIRESTORE_EMULATOR_HOST` が必須。プロジェクト ID は省略時 `firelang-test`):

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=firelang-test
cargo run --features emulator --bin fl -- --backend emulator path/to/script.fl
```

`fl --backend emulator` は feature なしで実行すると、SDK を読み込まずに
`emulator` feature の有効化方法を案内して終了します。in-memory 専用の
consumer は `cargo build --no-default-features` でも同じ in-memory API を利用できます。

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start --only firestore` など)。リポジトリ直下の `firebase.json` を使ってワンショットで試す例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator -- --nocapture'
```

本番 Firestore(認証付き)は非対応です。詳細は [仕様の 11-12 章](docs/spec.md#11-バックエンド) を参照してください。

## テスト

```bash
cargo test --all-targets            # ユニット + 統合テスト
cargo test --test example_scripts   # サンプルスクリプト一括実行
cargo test --test cli               # CLI スモークテスト
cargo test --test release           # release tag/package の検証

# 既定 feature なし: in-memory のみ
cargo check --no-default-features --all-targets
cargo test --no-default-features --all-targets

# emulator SDK を有効にしたビルド
cargo check --features emulator --all-targets
cargo test --features emulator --all-targets
```

エミュレータ統合テストは `FIRESTORE_EMULATOR_HOST` が未設定なら早期 return でスキップされます。`emulator` feature を有効にして、実際に叩くには上記の `firebase emulators:exec` を使うか、エミュレータ起動後に環境変数を設定して:

```bash
cargo test --features emulator --test firestore_emulator -- --nocapture
```

`Firestore` / `InMemoryFirestore` / `run_script` は feature に依存せず利用できます。
`FirestoreEmulator` と `fl --backend emulator` は `emulator` feature でのみ公開・有効です。
ライブラリとして emulator backend を使う consumer は、依存宣言に
`features = ["emulator"]` を指定します。

## CI / CD (GitHub Actions)

- **`test` ジョブ**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo build --release --bin fl` を実行
- **`feature-builds` ジョブ**: default / `--no-default-features` / `--features emulator` のビルド境界を検証
- **`integration-emulator` ジョブ**: Firestore エミュレータを起動して `cargo test --features emulator --test firestore_emulator` を実行
- CI は reusable workflow になっており、`v*` タグを押した CD も同じジョブ群（integration test を含む）を必ず実行します。CD の release ジョブはこの gate と tag version check に依存するため、branch CI を飛ばした tag でも公開できません。
- **CD**: tag の `v` を除いた部分（例: `v0.1.0` → `0.1.0`）が `cargo metadata --no-deps` の `firelang` package version と一致する場合だけ、`fl` の Linux バイナリを `--features emulator` でビルドして release します。
- release asset は raw binary ではなく `firelang-<version>-linux-x86_64.tar.gz` と SHA-256 `.sha256` sidecar です。tarball には実行権限付きの `fl`、`README.md`、`LICENSE`、`docs/spec.md`、ファイル単位の `SHA256SUMS` が含まれます。

ダウンロード後は、archive とその sidecar を同じディレクトリに置いて次のように検証できます。

```bash
sha256sum -c firelang-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf firelang-0.1.0-linux-x86_64.tar.gz
(cd firelang-0.1.0-linux-x86_64 && sha256sum -c SHA256SUMS)
```

## ライセンス

[MIT](LICENSE)
