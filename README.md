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

エミュレータは Firebase CLI で起動します(別ターミナルで `firebase emulators:start --only firestore` など)。リポジトリ直下の `firebase.json` を使ってワンショットで試す例:

```bash
firebase emulators:exec --only firestore --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'
```

本番 Firestore(認証付き)は非対応です。詳細は [仕様の 11-12 章](docs/spec.md#11-バックエンド) を参照してください。

## 信頼境界と実行予算

`fl` は任意のファイルをスクリプトとして受け取ります。無限再帰・無限ループ・巨大なネスト・巨大なクエリ結果がスタック枯渇や無期限の停止につながらないよう、**ランタイムは既定で有限の実行予算**を持ち、超過すると `error: <line>:<col>: ...` の通常のエラーで終了します(panic / OS kill にはなりません)。

既定値は「ライブラリ埋め込み」と `fl` CLI の両方に適用されます。

| 予算 | 既定値 | 対象 |
|------|--------|------|
| 呼び出し深度 | 64 | ネストした呼び出し(ユーザー関数・組み込み) |
| ステップ数 | 1,000,000 | 評価した文の数 |
| ループ反復数 | 100,000 | `while` / `each` / `map` の反復回数(実行全体で共有) |
| 壁時計時間 | 5,000 ms | 字句解析・構文解析・評価の経過時間 |
| パーサ深度 | 128 | 括弧・リテラル・`else if` チェーン等のネスト |
| 補間深度 | 32 | ネストした `"#{...}"` |
| 値ネスト深度 | 64 | 実行時 `Value` のネスト |
| 値ノード数 | 100,000 | 実行時 `Value` の総ノード数 |
| クエリ結果数 | 10,000 | 1 回のクエリで materialize される候補件数 |
| ソース長 | 1 MiB | UTF-8 バイト数 |
| トークン数 | 100,000 | 補間内を含む字句数 |
| バックエンド timeout | 5,000 ms | Firestore 1 回の呼び出し(組み込み emulator が強制) |

予算は embedding API から設定できます。

```rust
use std::time::Duration;
use firelang::{InMemoryFirestore, Runtime, RuntimeLimits};

let limits = RuntimeLimits::default()
    .with_max_call_depth(64)
    .with_max_wall_clock(Duration::from_millis(500));

let mut rt = Runtime::with_limits(InMemoryFirestore::default(), limits);
let out = rt.run("col(\"users\").limit(10).get()")?;

// 消費量はテストから観測できます。
let usage = rt.usage();
println!("steps={} loops={} max_depth={}", usage.steps, usage.loop_iterations, usage.max_call_depth);
```

`Runtime::new_unlimited` / `RuntimeLimits::unlimited()` は**信頼済みスクリプト向けの明示的なオプトイン**です。ユーザー入力・アップロードファイル・CLI には使わず、既定値のままにしてください。

> **スタックに関する注意**: 呼び出し深度の予算は Rust のネイティブスタックを保護します。1 段のユーザー関数呼び出しは debug ビルドで約 24 KiB のスタックを消費するため、既定値 64 は 2 MiB のスタック(テストスレッドなど)でも安全です。もっと小さいスタックのホストに埋め込む場合は `with_max_call_depth` でさらに下げてください。逆に予算を無効化すると、深い再帰でスタックオーバーフロー(プロセスの abort)になる可能性が戻ります。

ネットワーク等のバックエンド実装が応答しない場合、汎用の同期 `Firestore` トレイトでは強制中断できないため、組み込み `FirestoreEmulator` のみ timeout を強制します。

CLI では各予算を個別に上書きできます(`fl --help` に一覧):

```bash
fl --max-call-depth 64 --max-steps 100000 script.fl
fl --max-query-results 100 -e 'col("users").get()'
```

## テスト

```bash
cargo test                          # ユニット + in-memory テスト
cargo test --test budgets           # 実行予算(issue #82)のテスト
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
