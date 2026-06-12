# Firelang Refactoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** firelang コードベース全体の無駄(未使用依存・重複ロジック・責務混在・二重化した spec)を、外部から見える挙動を保ったままフェーズ分けで除去する。

**Architecture:** 現在 `runtime.rs`(1147行)に同居する「値モデル / Firestore トレイト / in-memory バックエンド / インタプリタ / ビルトイン」を `value.rs` / `firestore/{mod,memory,emulator}.rs` / `runtime.rs` に分離する。write のステージング(バッファリング)は両バックエンドに重複実装されているため Runtime 側(ExecCtx)に一本化し、`Firestore` トレイトを 13 → 7 メソッドに縮小する。spec は `docs/spec.md` を唯一の正とし、README は概要+リンクに薄くする。

**Tech Stack:** Rust (edition 2021), chrono, firestore crate (gRPC, emulator), tokio, GitHub Actions

---

## 現状分析(2026-06-12 時点のベースライン)

- `cargo test --all-targets`: 46 テスト全 green(emulator 統合テスト 10 件は `FIRESTORE_EMULATOR_HOST` 未設定時スキップ)
- `cargo fmt --check`: clean
- `cargo clippy --all-targets`: 警告 2 件(`type_complexity` @ parser.rs:271、`manual_is_multiple_of` @ runtime.rs:945)

### 発見した無駄・問題(→ 対応フェーズ)

| # | 問題 | 場所 | Phase |
|---|------|------|-------|
| 1 | spec が README(MVP Contract, 英語)+ docs/mvp/(完了済み計画資料, 英語)+ README 本文(日本語)に分散・重複。docs/mvp は実装と乖離(REST/JSON 前提の記述が残るが実装は gRPC SDK) | README.md, docs/mvp/ | 1(実施済) |
| 2 | `serde`(derive 付き)が完全に未使用 | Cargo.toml | 2 |
| 3 | `chrono` が依存にあるのに RFC3339 パーサを約 45 行手書き(`parse_rfc3339_to_unix` + `days_from_civil`) | runtime.rs:1000-1037 | 2 |
| 4 | 未公開・未文書のビルトイン `map(k, v, ...)`(map リテラル `{}` と重複) | runtime.rs:632, 944-957 | 2 |
| 5 | clippy 警告 2 件、CI に clippy ゲートなし | parser.rs, runtime.rs, ci.yml | 2 |
| 6 | runtime.rs(1147 行)に 5 責務が同居:値モデル / Firestore トレイト / InMemoryFirestore / インタプリタ / ビルトイン群 | src/runtime.rs | 3 |
| 7 | lib.rs の 6 割(165 行)がテストコード。公開 API の re-export が不揃い(`FirestoreEmulator` のみ crate 直下、`Value` は `runtime::` 経由) | src/lib.rs | 3 |
| 8 | write ステージングが両バックエンドに重複実装:`InMemoryMode` と `EmulatorMode` がほぼ同型(Batch(Vec<WriteOp>) / Transaction)、`batch_stage`/`transaction_stage`/`abort_batch` と「internal error: X while in batch/transaction」防御チェック 8 箇所が二重化。トレイト 13 メソッド | runtime.rs:96-335, emulator.rs:27-33, 198-398 | 4 |
| 9 | `set`/`update`/`delete` ビルトインが同一の ExecCtx 3-way match を三重に持つ | runtime.rs:658-705 | 4 |
| 10 | `where`/`limit`/`get` メソッドディスパッチが CollectionRef / CollectionGroupRef / QueryRef に三重化(約 85 行) | runtime.rs:769-853 | 5 |
| 11 | 非一貫: collection 直の `.limit()` だけ batch/tx 内でエラー(`where` はエラーにならない)。クエリ「構築」は禁止対象でないはず(禁止すべきは `.get()` 実行のみ) | runtime.rs:788-796, 814-822 | 5 |
| 12 | where 演算子が stringly-typed(`op: String`)。in-memory は未知演算子を黙って false(全件除外)、emulator はエラー — バックエンドで挙動が割れる | runtime.rs:60-65, 388-402; emulator.rs:465-477 | 5 |
| 13 | 文字列補間が eval 時に毎回 re-lex + re-parse(ループ内で毎周パース)。runtime → lexer/parser への逆依存。`#{` をエスケープする手段がない | runtime.rs:889-941 | 6 |
| 14 | map リテラルのキー内 `#{}` は補間されず黙ってリテラル扱い(値とは非一貫) | parser.rs:231-241 | 6 |
| 15 | examples/09 が 01 と同一内容(コメントのみ違う)で emulator スモークとして無意味 | examples/scripts/09_emulator_smoke.fl | 7 |

### 触らないもの(backlog、今回のリファクタリングのスコープ外)

YAGNI 原則により今回は実施しない。必要になったときに別プランを立てること。

- エラー型の構造化(行番号・カラム付きエラー)。現状の `Box<dyn Error>` + 文字列のまま
- Float 対応、`order_by`、カーソル/オフセット、本番 Firestore 認証(MVP contract でスコープ外)
- ドキュメントパスのセグメント数検証(現状 `set("users", ...)` は通る。挙動未定義として spec に明記済み)
- emulator の `abort_transaction` がロールバック RPC を送らない件(サーバ側タイムアウト任せ。現行と同じ)
- `puts` / `fl` 出力の Debug 形式(`Map({"a": Int(1)})`)の人間向け整形
- lexer の 1 文字トークン match のテーブル化(現状で十分読める)

---

## フェーズ概要

| Phase | 内容 | リスク | 規模 |
|-------|------|--------|------|
| 0 | 挙動ピン留めテスト追加(リファクタ前のガードレール) | なし | 小 |
| 1 | spec/docs クリーンアップ(**実施済み — 本セッション**) | なし | 中 |
| 2 | 依存・即効クリーンアップ(serde 削除、chrono 化、map() 削除、clippy) | 低 | 小 |
| 3 | モジュール分割(機械的移動、挙動不変) | 低 | 中 |
| 4 | Firestore トレイト簡素化(ステージングを Runtime に一本化) | 中 | 大 |
| 5 | ランタイム重複排除(クエリ統合、QueryOp enum、一貫性修正) | 低 | 中 |
| 6 | 文字列補間のパース時化 + `\#{` エスケープ | 中 | 中 |
| 7 | 仕上げ(examples、spec 同期確認) | なし | 小 |

各フェーズ完了時点で `cargo test --all-targets` green を維持する。フェーズは独立してマージ可能。

**実行前提:**

- 作業ディレクトリ: リポジトリルート
- 各タスクの最後に `cargo fmt && cargo test --all-targets` を実行してからコミットする
- emulator 統合テストをローカルで回す場合: `firebase emulators:exec --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'`(Phase 4 完了時は必ず実行すること。それ以外はホスト未設定スキップで可)

---

## Phase 0: 挙動ピン留めテスト

リファクタリングで触る箇所の現行挙動を、先にテストで固定する。すべて**現行コードで pass する**テスト(失敗したら現状理解が間違っているので止まって調査)。

### Task 0.1: in-memory の batch/transaction 挙動ピン留め

**Files:**
- Create: `tests/in_memory.rs`

- [ ] **Step 1: ピン留めテストを書く**

```rust
//! In-memory backend behavior tests (language + Firestore API).

use firelang::run_script;
use firelang::runtime::{InMemoryFirestore, Runtime, Value};

fn run_in(
    runtime: &mut Runtime<InMemoryFirestore>,
    source: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let tokens = firelang::lexer::lex(source)?;
    let program = firelang::parser::Parser::new(tokens).parse_program()?;
    runtime.eval_program(&program)
}

#[test]
fn batch_rejects_get() {
    let err = run_script("batch { get(\"users/x\") }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside batch"));
}

#[test]
fn batch_with_failing_update_persists_nothing() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "batch { set(\"users/b1\", {x: 1})\nupdate(\"users/missing\", {y: 2}) }",
    );
    assert!(failing.is_err());
    assert_eq!(run_in(&mut runtime, "get(\"users/b1\")").unwrap(), Value::Nil);
}

#[test]
fn nested_batch_is_rejected() {
    let err = run_script("batch { batch { set(\"users/n\", {x: 1}) } }");
    assert!(err.is_err());
}

#[test]
fn nested_transaction_is_rejected() {
    let err = run_script("transaction { transaction { set(\"users/n\", {x: 1}) } }");
    assert!(err.is_err());
}

#[test]
fn transaction_get_reads_snapshot_and_commit_applies_writes() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    run_in(&mut runtime, "set(\"users/s1\", {n: 1})").unwrap();
    let out = run_in(
        &mut runtime,
        "transaction { v = get(\"users/s1\")\nset(\"users/s1\", {n: 2})\nv }",
    )
    .unwrap();
    assert!(format!("{out:?}").contains("Int(1)"), "{out:?}");
    let after = run_in(&mut runtime, "get(\"users/s1\")").unwrap();
    assert!(format!("{after:?}").contains("Int(2)"), "{after:?}");
}

#[test]
fn transaction_rolls_back_on_runtime_error() {
    let mut runtime = Runtime::new(InMemoryFirestore::default());
    let failing = run_in(
        &mut runtime,
        "transaction { set(\"users/tx_rb\", {x: 1})\nundefined_var }",
    );
    assert!(failing.is_err());
    assert_eq!(run_in(&mut runtime, "get(\"users/tx_rb\")").unwrap(), Value::Nil);
}

#[test]
fn query_get_is_rejected_inside_batch_and_transaction() {
    let err = run_script("batch { collection(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside batch"));
    let err = run_script("transaction { collection(\"users\").get() }").unwrap_err();
    assert!(format!("{err}").contains("not allowed inside transaction"));
}

#[test]
fn update_missing_document_fails_in_memory() {
    let err = run_script("update(\"users/nope\", {x: 1})").unwrap_err();
    assert!(format!("{err}").contains("document not found"));
}
```

- [ ] **Step 2: 全テストが現行コードで pass することを確認**

Run: `cargo test --test in_memory`
Expected: `8 passed; 0 failed`(1 つでも fail したら現状理解が誤り。止まって調査)

- [ ] **Step 3: コミット**

```bash
git add tests/in_memory.rs
git commit -m "test: pin current batch/transaction behavior before refactoring"
```

---

## Phase 1: spec/docs クリーンアップ【実施済み — 2026-06-12 セッション】

実施内容(executor は確認のみでよい):

- `docs/spec.md` を新規作成。言語仕様 + Firestore API + backend semantics の**唯一の正式仕様**。README の MVP Contract と docs/mvp/tasks/01 の Decisions To Lock を統合し、コードから検証した未文書挙動(`__name__`、truthiness、括弧なし呼び出し、負リテラル非対応など)と既知の非一貫(本プラン #11, #12, #13, #14)を明記
- `README.md` を概要 + 使い方 + spec へのリンクに縮小(契約の重複記述を削除)
- `docs/mvp/` → `docs/archive/mvp/` へ移動(全タスク Done の歴史的計画資料。実装と乖離した記述があるため正とみなさない)。`task-list.md` 先頭に ARCHIVED バナー追加

**以後の全フェーズで挙動・契約を変える場合は `docs/spec.md` を同じコミットで更新すること**(各タスクに明記済み)。

---

## Phase 2: 依存・即効クリーンアップ

### Task 2.1: 未使用依存 serde の削除

**Files:**
- Modify: `Cargo.toml:8`

- [ ] **Step 1: serde 行を削除**

```toml
[dependencies]
chrono = { version = "0.4", default-features = false, features = ["std"] }
firestore = "0.49"
gcloud-sdk = "0.30"
prost-types = "0.14"
tokio = { version = "1", features = ["net", "rt", "time"] }
```

(`serde = { version = "1.0", features = ["derive"] }` の行を消すだけ。使用箇所ゼロは `rg serde src/ tests/` で確認済み)

- [ ] **Step 2: 検証してコミット**

Run: `cargo test --all-targets`
Expected: 全 green(Cargo.lock も更新される)

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: remove unused serde dependency"
```

### Task 2.2: 手書き RFC3339 パーサを chrono に置換

**Files:**
- Modify: `src/runtime.rs:1000-1037`(`parse_rfc3339_to_unix` と `days_from_civil`)
- Test: `tests/in_memory.rs`

- [ ] **Step 1: 失敗ケースのテストを先に書く**(`tests/in_memory.rs` に追記)

```rust
#[test]
fn timestamp_rejects_invalid_rfc3339() {
    let err = run_script("timestamp(\"2026-13-99T99:99:99Z\")").unwrap_err();
    assert!(format!("{err}").contains("invalid RFC3339"), "{err}");
}
```

Run: `cargo test --test in_memory timestamp_rejects`
Expected: PASS(現行実装も "invalid RFC3339" を返す。これは置換後もメッセージ互換を保つためのピン)

- [ ] **Step 2: 置換**

`src/runtime.rs` の `parse_rfc3339_to_unix`(27 行)と `days_from_civil`(10 行)を以下の 5 行に置き換える:

```rust
fn parse_rfc3339_to_unix(s: &str) -> Result<i64, Box<dyn Error>> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| format!("invalid RFC3339 timestamp: {e}"))?
        .timestamp())
}
```

挙動差分(許容): chrono は小数秒付き(`...T00:00:00.5+09:00`)も受理する(現行はエラー)。受理範囲が広がる方向のみで、既存の受理入力の結果は不変(`lib.rs` の `parses_timestamp_from_rfc3339_string` が 1769871600 でピンしている)。

- [ ] **Step 3: 検証してコミット**

Run: `cargo test --all-targets`
Expected: 全 green

```bash
git add src/runtime.rs tests/in_memory.rs
git commit -m "refactor: replace hand-rolled RFC3339 parser with chrono"
```

### Task 2.3: 未公開ビルトイン map() の削除

`{key: value}` リテラルと完全に重複し、README/spec のどこにも文書化されていない。使用箇所なし(`rg -l 'map\(' examples/ tests/` で確認済み — `docs/spec.md` の既知の制限にも削除予定と記載済み)。

**Files:**
- Modify: `src/runtime.rs:632`(`"map" => map_from_kv(&args),` の行)と `src/runtime.rs:944-957`(`map_from_kv` 関数)

- [ ] **Step 1: 削除**

`eval_call` の match から `"map" => map_from_kv(&args),` のアームを削除し、`map_from_kv` 関数(14 行)を削除する。

- [ ] **Step 2: 検証してコミット**

Run: `cargo test --all-targets`
Expected: 全 green(`map(...)` は以後 "undefined function: map")

`docs/spec.md` の「既知の制限」から map() の項を削除する。

```bash
git add src/runtime.rs docs/spec.md
git commit -m "refactor: remove undocumented map() builtin (redundant with map literals)"
```

### Task 2.4: clippy 警告解消 + CI に clippy ゲート追加

**Files:**
- Modify: `src/parser.rs:271`
- Modify: `.github/workflows/ci.yml`(test ジョブ)
- Modify: `docs/release-checklist.md`

- [ ] **Step 1: parser.rs の type_complexity を type alias で解消**

`Parser` impl の上(`src/parser.rs:5` 付近)に追加:

```rust
type ParsedArgs = (Vec<Expr>, Vec<(String, Expr)>);
```

`parse_args` のシグネチャを変更:

```rust
fn parse_args(&mut self) -> Result<ParsedArgs, Box<dyn Error>> {
```

(`manual_is_multiple_of` 警告は Task 2.3 の `map_from_kv` 削除で消えている)

- [ ] **Step 2: clippy が警告ゼロなことを確認**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: エラー・警告なしで終了

- [ ] **Step 3: CI の test ジョブに clippy ステップを追加**

`.github/workflows/ci.yml` の `Format check` ステップの直後に:

```yaml
      - name: Clippy
        run: cargo clippy --all-targets -- -D warnings
```

`docs/release-checklist.md` の Local セクションに `cargo clippy --all-targets -- -D warnings` を追記。

- [ ] **Step 4: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green、警告なし

```bash
git add src/parser.rs .github/workflows/ci.yml docs/release-checklist.md
git commit -m "chore: fix clippy warnings and gate CI on clippy"
```

---

## Phase 3: モジュール分割(挙動不変)

方針: 機械的な移動のみ。ロジックは一切変えない。各タスク後にテスト green を確認。後方互換 shim(旧パスの re-export 残し)は作らない — 利用側(lib.rs / emulator.rs / tests)を直接書き換える。

### Task 3.1: 値モデルを src/value.rs へ抽出

**Files:**
- Create: `src/value.rs`
- Modify: `src/runtime.rs`, `src/lib.rs`, `src/emulator.rs`

- [ ] **Step 1: src/value.rs を作成し、runtime.rs から以下を移動**

移動する項目(runtime.rs から該当定義をカット&ペースト、`pub` はそのまま):

- `Value`(enum)
- `Timestamp`, `DocRef`, `CollectionRef`, `CollectionGroupRef`(struct)
- `QueryRef`, `QuerySource`, `QueryFilter`
- `UserFunc`
- `render` 関数 → `impl fmt::Display for Value` に変換(下記)
- `truthy` 関数 → `impl Value { pub fn truthy(&self) -> bool }` に変換
- `current_unix_seconds` 関数 → `impl Timestamp { pub fn now() -> Result<Self, Box<dyn Error>> }` に変換

新規に書くコード(変換部分):

```rust
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

impl Timestamp {
    pub fn now() -> Result<Self, Box<dyn Error>> {
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Ok(Self { seconds })
    }
}

impl Value {
    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Nil)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Nil => write!(f, "nil"),
            Value::Array(a) => write!(f, "{a:?}"),
            Value::Map(m) => write!(f, "{m:?}"),
            Value::Timestamp(ts) => write!(f, "Timestamp({})", ts.seconds),
            Value::ServerTimestamp => write!(f, "ServerTimestamp"),
            Value::ArrayUnion(v) => write!(f, "ArrayUnion({v:?})"),
            Value::ArrayRemove(v) => write!(f, "ArrayRemove({v:?})"),
            Value::DocRef(d) => write!(f, "#<DocRef path={}>", d.path),
            Value::CollectionRef(c) => write!(f, "#<CollectionRef path={}>", c.path),
            Value::CollectionGroupRef(cg) => write!(f, "#<CollectionGroupRef id={}>", cg.id),
            Value::QueryRef(_) => write!(f, "#<QueryRef>"),
            Value::Func(_) => write!(f, "<func>"),
        }
    }
}
```

- [ ] **Step 2: 利用側を更新**

- `src/runtime.rs`: 先頭に `use crate::value::{CollectionGroupRef, CollectionRef, DocRef, QueryFilter, QueryRef, QuerySource, Timestamp, UserFunc, Value};` を追加。`render(&v)` 呼び出し(`puts`、補間)を `format!("{v}")` / `v.to_string()` に、`truthy(&x)` を `x.truthy()` に、`current_unix_seconds()?` を `Timestamp::now()?.seconds`(`now` ビルトインと `resolve_sentinel_map` は `Value::Timestamp(Timestamp::now()?)`)に置換
- `src/lib.rs`: `pub mod value;` を追加し、`pub use value::Value;` を追加
- `src/emulator.rs`: `use crate::runtime::{Firestore, QueryFilter, Timestamp, Value, WriteOp};` → `use crate::runtime::{Firestore, WriteOp}; use crate::value::{QueryFilter, Timestamp, Value};`
- `src/runtime.rs` に残す: `Firestore` トレイト、`WriteOp`、`InMemoryFirestore` 一式、`Runtime`、`ExecCtx`、`ExecFlow`、`expect_*`、`eval_binary`、`bind_function_args`、`parse_timestamp_arg`、`parse_rfc3339_to_unix`

- [ ] **Step 3: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green

```bash
git add src/value.rs src/runtime.rs src/lib.rs src/emulator.rs
git commit -m "refactor: extract value model into src/value.rs"
```

### Task 3.2: Firestore トレイトと in-memory バックエンドを src/firestore/ へ

**Files:**
- Create: `src/firestore/mod.rs`, `src/firestore/memory.rs`
- Modify: `src/runtime.rs`, `src/lib.rs`, `src/emulator.rs`

- [ ] **Step 1: src/firestore/mod.rs を作成**

runtime.rs から `Firestore` トレイト(現 13 メソッドのまま)と `WriteOp` を移動:

```rust
//! Firestore backend abstraction.
mod memory;

pub use memory::InMemoryFirestore;

use std::collections::BTreeMap;
use std::error::Error;

use crate::value::{QueryFilter, Value};

// (WriteOp と trait Firestore を runtime.rs からそのまま移動)
```

- [ ] **Step 2: src/firestore/memory.rs を作成**

runtime.rs から以下を**そのまま**移動: `InMemoryMode`, `InMemoryFirestore`, `impl Firestore for InMemoryFirestore`, `apply_write_to_docs`, `wrap_doc`, `take_matches`, `matches_filters`, `compare_int`, `apply_update_map`, `resolve_sentinel_map`。

先頭の use:

```rust
use std::collections::{BTreeMap, HashMap};
use std::error::Error;

use super::{Firestore, WriteOp};
use crate::value::{QueryFilter, Timestamp, Value};
```

- [ ] **Step 3: 利用側を更新**

- `src/lib.rs`: `pub mod firestore;` を追加、`use runtime::{Firestore, InMemoryFirestore, Runtime, Value};` → `use firestore::{Firestore, InMemoryFirestore}; use runtime::Runtime; use value::Value;`、`pub use firestore::{Firestore, InMemoryFirestore};` を追加
- `src/runtime.rs`: `use crate::firestore::{Firestore, WriteOp};` を追加(削除した定義の参照を差し替え)
- `src/emulator.rs`: `use crate::runtime::{Firestore, WriteOp};` → `use crate::firestore::{Firestore, WriteOp};`
- `src/lib.rs` 内テスト(まだ移動前)と `tests/in_memory.rs` の `firelang::runtime::{InMemoryFirestore, ...}` → `InMemoryFirestore` は `firelang::firestore::` 経由(または crate 直下 re-export)に更新

- [ ] **Step 4: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green

```bash
git add src/firestore/ src/runtime.rs src/lib.rs src/emulator.rs tests/in_memory.rs
git commit -m "refactor: move Firestore trait and in-memory backend into src/firestore/"
```

### Task 3.3: emulator を src/firestore/emulator.rs へ移動

**Files:**
- Move: `src/emulator.rs` → `src/firestore/emulator.rs`
- Modify: `src/firestore/mod.rs`, `src/lib.rs`

- [ ] **Step 1: 移動**

```bash
git mv src/emulator.rs src/firestore/emulator.rs
```

`src/firestore/mod.rs` に追加:

```rust
mod emulator;

pub use emulator::FirestoreEmulator;
```

`src/firestore/emulator.rs` 内: `use crate::firestore::{Firestore, WriteOp};` → `use super::{Firestore, WriteOp};`

`src/lib.rs`: `pub mod emulator;` を削除し、`pub use emulator::FirestoreEmulator;` → `pub use firestore::FirestoreEmulator;`

- [ ] **Step 2: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green

```bash
git add -A src/ && git commit -m "refactor: move emulator backend under src/firestore/"
```

### Task 3.4: lib.rs のテストを tests/ へ移動し、公開 API を整える

**Files:**
- Modify: `src/lib.rs`(28-193 行のテスト mod を削除)
- Modify: `tests/in_memory.rs`(lib.rs のテストを統合)
- Modify: `tests/example_scripts.rs`

- [ ] **Step 1: lib.rs の `#[cfg(test)] mod tests` の中身 19 テストを tests/in_memory.rs に移動**

import を整える(tests/in_memory.rs 冒頭):

```rust
use firelang::run_script;
use firelang::runtime::Runtime;
use firelang::{InMemoryFirestore, Value};
```

`run_in` ヘルパーは Task 0.1 で既に存在するので、lib.rs 側の重複定義は捨てる。

- [ ] **Step 2: lib.rs を最終形にする(全文、約 30 行)**

```rust
pub mod ast;
pub mod firestore;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod value;

pub use firestore::{Firestore, FirestoreEmulator, InMemoryFirestore};
pub use value::Value;

use std::error::Error;

use runtime::Runtime;

pub fn run_script(source: &str) -> Result<Value, Box<dyn Error>> {
    run_script_with_firestore(source, InMemoryFirestore::default())
}

pub fn run_script_with_firestore<F: Firestore>(
    source: &str,
    firestore: F,
) -> Result<Value, Box<dyn Error>> {
    let tokens = lexer::lex(source)?;
    let mut parser = parser::Parser::new(tokens);
    let program = parser.parse_program()?;
    let mut runtime = Runtime::new(firestore);
    runtime.eval_program(&program)
}
```

- [ ] **Step 3: tests/example_scripts.rs の import を更新**

`use firelang::runtime::Value;` → `use firelang::Value;`

- [ ] **Step 4: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green(テスト総数は移動前と同じ。lib.rs unit 19 + in_memory 9 → in_memory 28)

```bash
git add src/lib.rs tests/
git commit -m "refactor: move lib.rs tests to tests/in_memory.rs and clean up public API"
```

---

## Phase 4: Firestore トレイト簡素化(ステージングを Runtime に一本化)

現状、write のバッファリング(ステージング)を InMemoryFirestore と FirestoreEmulator が**それぞれ**`mode` 状態機械として持つ(問題 #8)。しかし「いまバッチ/トランザクション中か」は Runtime の `ExecCtx` が既に知っている。バッファを Runtime に置けば、トレイトは 13 → 7 メソッドになり、両バックエンドの mode enum・stage/abort メソッド・防御チェック計約 150 行が消える。

### Task 4.1: 新トレイト定義と InMemoryFirestore の書き換え

**Files:**
- Modify: `src/firestore/mod.rs`
- Modify: `src/firestore/memory.rs`

- [ ] **Step 1: トレイトを置き換える**(`src/firestore/mod.rs`)

```rust
use crate::value::{QueryRef, Value};

pub trait Firestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    /// Apply writes atomically: either all writes persist or none do.
    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    fn run_query(&self, query: &QueryRef) -> Result<Vec<Value>, Box<dyn Error>>;

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>>;
    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>>;
    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>>;
    fn abort_transaction(&mut self);
}
```

(`set`/`update`/`delete`/`query_collection`/`query_collection_group`/`begin_batch`/`batch_stage`/`commit_batch`/`abort_batch`/`get_in_transaction`/`transaction_stage` は削除。単発 write は `apply(vec![op])`、batch は Runtime がバッファして `apply(ops)` で一括)

- [ ] **Step 2: InMemoryFirestore を書き換える**(`src/firestore/memory.rs`)

`InMemoryMode` enum を削除し、struct と impl を以下に置き換える(ヘルパー関数 `apply_write_to_docs` / `wrap_doc` / `take_matches` / `matches_filters` / `compare_int` / `apply_update_map` / `resolve_sentinel_map` は変更なし):

```rust
#[derive(Default)]
pub struct InMemoryFirestore {
    docs: HashMap<String, BTreeMap<String, Value>>,
    tx_snapshot: Option<HashMap<String, BTreeMap<String, Value>>>,
}

impl Firestore for InMemoryFirestore {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        Ok(self.docs.get(path).cloned().map(Value::Map))
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let mut next = self.docs.clone();
        for op in writes {
            apply_write_to_docs(&mut next, op)?;
        }
        self.docs = next;
        Ok(())
    }

    fn run_query(&self, query: &QueryRef) -> Result<Vec<Value>, Box<dyn Error>> {
        match &query.source {
            QuerySource::Collection(path) => {
                self.query_collection(path, &query.filters, query.limit)
            }
            QuerySource::CollectionGroup(id) => {
                self.query_collection_group(id, &query.filters, query.limit)
            }
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.tx_snapshot.is_some() {
            return Err("already in transaction".into());
        }
        self.tx_snapshot = Some(self.docs.clone());
        Ok(())
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        match &self.tx_snapshot {
            Some(snapshot) => Ok(snapshot.get(path).cloned().map(Value::Map)),
            None => Err("not in transaction".into()),
        }
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let mut next = self.tx_snapshot.take().ok_or("not in transaction")?;
        for op in writes {
            apply_write_to_docs(&mut next, op)?;
        }
        self.docs = next;
        Ok(())
    }

    fn abort_transaction(&mut self) {
        self.tx_snapshot = None;
    }
}
```

既存の `query_collection` / `query_collection_group` はトレイトメソッドから `impl InMemoryFirestore` の private メソッド(`fn query_collection(&self, ...)` のまま、`pub` なし)に移す。中身は不変。

use に `QueryRef`, `QuerySource` を追加: `use crate::value::{QueryFilter, QueryRef, QuerySource, Timestamp, Value};`

- [ ] **Step 3: コンパイルが emulator/runtime で壊れていることを確認**(次タスクで直す)

Run: `cargo build 2>&1 | head -5`
Expected: emulator.rs と runtime.rs のトレイト不一致エラー(この時点ではコミットしない)

### Task 4.2: FirestoreEmulator の書き換え

**Files:**
- Modify: `src/firestore/emulator.rs`

- [ ] **Step 1: EmulatorMode を削除し、struct と impl を書き換える**

struct(`mode: Option<EmulatorMode>` → `transaction_id: Option<Vec<u8>>`):

```rust
pub struct FirestoreEmulator {
    host: String,
    project_id: String,
    runtime: Runtime,
    db: RefCell<Option<FirestoreDb>>,
    transaction_id: Option<Vec<u8>>,
}
```

(`new` の初期化も `transaction_id: None` に。`commit_single_write` メソッドは削除)

impl Firestore:

```rust
impl Firestore for FirestoreEmulator {
    fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        // 現行の get と同一。冒頭の mode チェック (return Err("internal error: ...")) のみ削除
    }

    fn apply(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let sdk_writes = write_ops_to_sdk_writes(self, &writes)?;
        self.commit_writes(sdk_writes, vec![])
    }

    fn run_query(&self, query: &QueryRef) -> Result<Vec<Value>, Box<dyn Error>> {
        match &query.source {
            QuerySource::Collection(path) => {
                let (parent, collection_id) = split_collection_path(path)?;
                let sq = structured_query(collection_id, false, &query.filters)?;
                run_query_sorted(self, parent.as_deref(), sq, query.limit)
            }
            QuerySource::CollectionGroup(id) => {
                let sq = structured_query(id, true, &query.filters)?;
                run_query_sorted(self, None, sq, query.limit)
            }
        }
    }

    fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
        if self.transaction_id.is_some() {
            return Err("already in transaction".into());
        }
        // 現行 begin_transaction の RPC 部分は不変。最後だけ:
        self.transaction_id = Some(transaction_id);
        Ok(())
    }

    fn transaction_get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let tid = self
            .transaction_id
            .clone()
            .ok_or("not in transaction")?;
        // 現行 get_in_transaction の RPC 部分は不変(tid の取得元だけ変更)
    }

    fn commit_transaction(&mut self, writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
        let tid = self.transaction_id.take().ok_or("not in transaction")?;
        let sdk_writes = write_ops_to_sdk_writes(self, &writes)?;
        self.commit_writes(sdk_writes, tid)
    }

    fn abort_transaction(&mut self) {
        self.transaction_id = None;
    }
}
```

(set/update/delete/query_collection/query_collection_group/begin_batch/batch_stage/commit_batch/abort_batch/transaction_stage の各メソッドと、その中の mode 防御チェックはすべて削除。エンコード/デコード系の自由関数は一切変更なし)

use 追加: `use crate::value::{QueryRef, QuerySource, ...};`

- [ ] **Step 2: ビルドが runtime.rs のみで壊れる状態を確認**(コミットはまだ)

Run: `cargo build 2>&1 | rg 'error' | head -5`

### Task 4.3: Runtime の ExecCtx と write ディスパッチを一本化

**Files:**
- Modify: `src/runtime.rs`
- Test: `tests/in_memory.rs`(Phase 0 のピンテストがそのまま検収基準)

- [ ] **Step 1: ExecCtx をバッファ持ちに変更**

```rust
enum ExecCtx {
    Normal,
    Batch { writes: Vec<WriteOp> },
    Transaction { writes: Vec<WriteOp> },
}
```

(`pub` と `derive(Copy, PartialEq)` は外す。`wrote: bool` は `!writes.is_empty()` で代替)

- [ ] **Step 2: dispatch_write ヘルパーを追加し、set/update/delete の三重 match を置き換える**

```rust
fn dispatch_write(&mut self, op: WriteOp) -> Result<(), Box<dyn Error>> {
    match &mut self.exec {
        ExecCtx::Normal => self.fs.apply(vec![op]),
        ExecCtx::Batch { writes } | ExecCtx::Transaction { writes } => {
            writes.push(op);
            Ok(())
        }
    }
}
```

ビルトイン側(例: set。update/delete も同様に末尾を `self.dispatch_write(...)` に):

```rust
"set" => {
    let path = expect_doc_path(args.first())?.to_string();
    let data = expect_map(args.get(1))?.clone();
    let merge = match (args.get(2), kwargs.get("merge")) {
        (_, Some(Value::Bool(v))) => *v,
        (Some(Value::Bool(v)), None) => *v,
        (None, None) => false,
        _ => return Err("set merge must be bool".into()),
    };
    self.dispatch_write(WriteOp::Set { path, data, merge })?;
    Ok(Value::Nil)
}
```

- [ ] **Step 3: get ビルトインを更新**

```rust
"get" => {
    let path = expect_doc_path(args.first())?;
    match &self.exec {
        ExecCtx::Batch { .. } => Err("get is not allowed inside batch".into()),
        ExecCtx::Transaction { writes } if !writes.is_empty() => {
            Err("get is not allowed after the first write in a transaction".into())
        }
        ExecCtx::Transaction { .. } => {
            Ok(self.fs.transaction_get(path)?.unwrap_or(Value::Nil))
        }
        ExecCtx::Normal => Ok(self.fs.get(path)?.unwrap_or(Value::Nil)),
    }
}
```

- [ ] **Step 4: batch / transaction ビルトインを書き換える**

`ExecFlow` にヘルパーを追加:

```rust
impl ExecFlow {
    fn into_value(self) -> Value {
        match self {
            ExecFlow::Value(v) | ExecFlow::Return(v) => v,
        }
    }
}
```

```rust
"batch" => {
    let Some(stmts) = block else {
        return Err("batch requires { ... } block".into());
    };
    if !matches!(self.exec, ExecCtx::Normal) {
        return Err("batch cannot be nested inside batch/transaction".into());
    }
    self.exec = ExecCtx::Batch { writes: vec![] };
    let flow = self.eval_block(stmts);
    let ExecCtx::Batch { writes } = std::mem::replace(&mut self.exec, ExecCtx::Normal) else {
        unreachable!("exec ctx changed inside batch");
    };
    let flow = flow?;
    self.fs.apply(writes)?;
    Ok(flow.into_value())
}
"transaction" => {
    let Some(stmts) = block else {
        return Err("transaction requires { ... } block".into());
    };
    if !matches!(self.exec, ExecCtx::Normal) {
        return Err("transaction cannot be nested inside batch/transaction".into());
    }
    self.fs.begin_transaction()?;
    self.exec = ExecCtx::Transaction { writes: vec![] };
    let flow = self.eval_block(stmts);
    let ExecCtx::Transaction { writes } =
        std::mem::replace(&mut self.exec, ExecCtx::Normal)
    else {
        unreachable!("exec ctx changed inside transaction");
    };
    match flow {
        Ok(flow) => {
            self.fs.commit_transaction(writes)?;
            Ok(flow.into_value())
        }
        Err(e) => {
            self.fs.abort_transaction();
            Err(e)
        }
    }
}
```

(batch 失敗時: バッファは捨てるだけでバックエンド呼び出し不要 — これが abort_batch が消える理由)

- [ ] **Step 5: クエリ実行の呼び出し先を差し替える**

`src/value.rs` に変換ヘルパーを追加:

```rust
impl CollectionRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::Collection(self.path.clone()),
            filters: vec![],
            limit: None,
        }
    }
}

impl CollectionGroupRef {
    pub fn to_query(&self) -> QueryRef {
        QueryRef {
            source: QuerySource::CollectionGroup(self.id.clone()),
            filters: vec![],
            limit: None,
        }
    }
}
```

`eval_method_call` 内の 3 箇所の `get`:

- CollectionRef: `self.fs.query_collection(&collection.path, &[], None)?` → `self.fs.run_query(&collection.to_query())?`
- CollectionGroupRef: 同様に `&group.to_query()`
- QueryRef: `match &query.source { ... }` の分岐ごと削除して `self.fs.run_query(&query)?`

`forbid_query_in_batch_or_transaction` の match を新 ExecCtx 形(`ExecCtx::Batch { .. }` / `ExecCtx::Transaction { .. }`)に合わせる。

- [ ] **Step 6: 検証**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green。特に Phase 0 のピンテスト 8 件と `batch_rolls_back_on_runtime_error` / `transaction_rejects_get_after_write` が green であること

- [ ] **Step 7: emulator 統合テストを実機で実行**

Run: `firebase emulators:exec --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'`
Expected: 10 passed(batch atomic commit / transaction read-then-write / update missing fails を含む)

- [ ] **Step 8: spec を更新してコミット**

`docs/spec.md` の「既知の制限・実装メモ」からトレイト二重化に関する記述があれば更新(ネスト時のエラーメッセージが "already in batch or transaction" から "batch/transaction cannot be nested..." に変わる旨は仕様上の文言変更として 8 章に反映)。

```bash
git add src/ docs/spec.md
git commit -m "refactor: move write staging into Runtime, shrink Firestore trait to 7 methods"
```

---

## Phase 5: ランタイム重複排除と一貫性修正

### Task 5.1: クエリメソッドディスパッチの統合

**Files:**
- Modify: `src/runtime.rs`(`eval_method_call`)
- Test: `tests/in_memory.rs`

- [ ] **Step 1: 挙動変更のテストを先に書く**(`tests/in_memory.rs` に追記)

```rust
#[test]
fn collection_limit_builds_query_inside_transaction() {
    // クエリの「構築」は batch/transaction 内でも許される。禁止されるのは .get() 実行のみ。
    let out = run_script(
        "q = collection(\"users\").limit(1)\ntransaction { q2 = collection(\"users\").limit(1)\nset(\"users/l1\", {n: 1}) }\nq",
    );
    assert!(out.is_ok(), "{out:?}");
}
```

Run: `cargo test --test in_memory collection_limit_builds`
Expected: **FAIL**(現行は "queries are not allowed inside transaction")

- [ ] **Step 2: eval_method_call を統合形に書き換える**

```rust
fn eval_method_call(
    &mut self,
    receiver: Value,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Box<dyn Error>> {
    match receiver {
        Value::CollectionRef(collection) if name == "doc" => Ok(Value::DocRef(DocRef {
            path: format!("{}/{}", collection.path, expect_string(args.first())?),
        })),
        Value::CollectionRef(collection) => {
            self.eval_query_method(collection.to_query(), name, args)
        }
        Value::CollectionGroupRef(group) => self.eval_query_method(group.to_query(), name, args),
        Value::QueryRef(query) => self.eval_query_method(query, name, args),
        Value::Timestamp(ts) => match name {
            "to_unix" => Ok(Value::Int(ts.seconds)),
            "add_seconds" => Ok(Value::Timestamp(Timestamp {
                seconds: ts.seconds + expect_int(args.first())?,
            })),
            _ => Err(format!("unknown timestamp method: {name}").into()),
        },
        _ => Err("unsupported method receiver".into()),
    }
}

fn eval_query_method(
    &mut self,
    mut query: QueryRef,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Box<dyn Error>> {
    match name {
        "where" => {
            query.filters.push(QueryFilter {
                field: expect_string(args.first())?.to_string(),
                op: expect_string(args.get(1))?.to_string(),
                value: args.get(2).cloned().ok_or("where requires value")?,
            });
            Ok(Value::QueryRef(query))
        }
        "limit" => {
            query.limit = Some(expect_int(args.first())? as usize);
            Ok(Value::QueryRef(query))
        }
        "get" => {
            self.forbid_query_in_batch_or_transaction()?;
            Ok(Value::Array(self.fs.run_query(&query)?))
        }
        _ => Err(format!("unknown query method: {name}").into()),
    }
}
```

意図した挙動変更(spec の 7 章・13 章を同コミットで更新):

1. `.limit()` / `.where()` は batch/tx 内でも可。`.get()` のみ禁止(問題 #11 解消)
2. エラーメッセージが "unknown collection method" / "unknown collection_group method" → "unknown query method" に統一

- [ ] **Step 3: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green(Step 1 のテストも green に)

```bash
git add src/runtime.rs src/value.rs tests/in_memory.rs docs/spec.md
git commit -m "refactor: unify query method dispatch; allow query building inside batch/tx"
```

### Task 5.2: QueryOp enum 化(where 演算子の検証を構築時に統一)

in-memory は未知演算子を黙って false、emulator はクエリ実行時エラー(問題 #12)。`where()` 呼び出し時に検証して enum 化し、両バックエンドの分岐を網羅 match にする。

**Files:**
- Modify: `src/value.rs`, `src/runtime.rs`, `src/firestore/memory.rs`, `src/firestore/emulator.rs`
- Test: `tests/in_memory.rs`

- [ ] **Step 1: テストを先に書く**

```rust
#[test]
fn where_rejects_unsupported_operator() {
    let err = run_script("collection(\"users\").where(\"a\", \"!=\", 1)").unwrap_err();
    assert!(format!("{err}").contains("unsupported query op"), "{err}");
}
```

Run: `cargo test --test in_memory where_rejects`
Expected: **FAIL**(現行 in-memory は黙って空配列になるため where 自体は成功する)

- [ ] **Step 2: src/value.rs に QueryOp を追加し、QueryFilter を変更**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

impl QueryOp {
    pub fn parse(s: &str) -> Result<Self, Box<dyn Error>> {
        Ok(match s {
            "==" => Self::Eq,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "<" => Self::Lt,
            "<=" => Self::Le,
            other => return Err(format!("unsupported query op: {other}").into()),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryFilter {
    pub field: String,
    pub op: QueryOp,
    pub value: Value,
}
```

- [ ] **Step 3: 利用側 3 箇所を更新**

`src/runtime.rs`(eval_query_method の where):

```rust
"where" => {
    query.filters.push(QueryFilter {
        field: expect_string(args.first())?.to_string(),
        op: QueryOp::parse(expect_string(args.get(1))?)?,
        value: args.get(2).cloned().ok_or("where requires value")?,
    });
    Ok(Value::QueryRef(query))
}
```

`src/firestore/memory.rs`(matches_filters):

```rust
match f.op {
    QueryOp::Eq => actual == &f.value,
    QueryOp::Gt => compare_int(actual, &f.value, |a, b| a > b),
    QueryOp::Ge => compare_int(actual, &f.value, |a, b| a >= b),
    QueryOp::Lt => compare_int(actual, &f.value, |a, b| a < b),
    QueryOp::Le => compare_int(actual, &f.value, |a, b| a <= b),
}
```

`src/firestore/emulator.rs`(build_where。エラーアームが消えるので戻り値の `?` 連鎖も単純化可):

```rust
let op = match f.op {
    QueryOp::Eq => structured_query::field_filter::Operator::Equal,
    QueryOp::Gt => structured_query::field_filter::Operator::GreaterThan,
    QueryOp::Ge => structured_query::field_filter::Operator::GreaterThanOrEqual,
    QueryOp::Lt => structured_query::field_filter::Operator::LessThan,
    QueryOp::Le => structured_query::field_filter::Operator::LessThanOrEqual,
};
```

- [ ] **Step 4: 検証、spec 更新、コミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green

`docs/spec.md` 7 章のサポート演算子表に「未対応演算子は `where()` 呼び出し時にエラー」と明記し、13 章の該当項(in-memory/emulator の挙動差)を削除。

```bash
git add src/ tests/in_memory.rs docs/spec.md
git commit -m "refactor: make query operators a typed enum, validated at where() call"
```

### Task 5.3: parser の引数パースループ重複排除(小)

**Files:**
- Modify: `src/parser.rs`(`parse_args`)

- [ ] **Step 1: 共通ヘルパーを抽出**

`parse_args` 内で 4 回繰り返される「kwarg か positional かを判定して push」を 1 ヘルパーに:

```rust
fn parse_arg_into(
    &mut self,
    args: &mut Vec<Expr>,
    kwargs: &mut Vec<(String, Expr)>,
) -> Result<(), Box<dyn Error>> {
    if let Some(kw) = self.parse_kwarg()? {
        kwargs.push(kw);
    } else {
        args.push(self.parse_expr()?);
    }
    Ok(())
}

fn parse_args(&mut self) -> Result<ParsedArgs, Box<dyn Error>> {
    let mut args = vec![];
    let mut kwargs = vec![];
    if self.at(&Token::LParen) {
        self.bump();
        if !self.at(&Token::RParen) {
            loop {
                self.parse_arg_into(&mut args, &mut kwargs)?;
                if self.at(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        return Ok((args, kwargs));
    }
    if self.is_arg_start() {
        loop {
            self.parse_arg_into(&mut args, &mut kwargs)?;
            if self.at(&Token::Comma) {
                self.bump();
            } else {
                break;
            }
        }
    }
    Ok((args, kwargs))
}
```

- [ ] **Step 2: 検証してコミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green

```bash
git add src/parser.rs
git commit -m "refactor: deduplicate argument parsing loops in parser"
```

---

## Phase 6: 文字列補間のパース時化

現状は eval のたびに文字列を re-lex + re-parse(while ループ内なら毎周)し、runtime が lexer/parser に逆依存している(問題 #13)。補間の分割を lexer に、埋め込み式のパースを parser に移す。副次効果として `\#{...}` エスケープが可能になり(現状は不可能)、map キー内の `#{}` を明示的にエラーにできる(問題 #14)。

### Task 6.1: lexer で文字列をセグメント化

**Files:**
- Modify: `src/lexer.rs`

- [ ] **Step 1: Token::Str の型を変更**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// `#{ ... }` の中身の生ソース。パースは parser が行う。
    Interp(String),
}
```

`Token::Str(String)` → `Token::Str(Vec<StrPart>)`

- [ ] **Step 2: 文字列スキャンを書き換える**(lexer.rs の `'"'` アーム全体を置換)

```rust
'"' => {
    chars.next();
    let mut parts: Vec<StrPart> = vec![];
    let mut lit = String::new();
    loop {
        let ch = chars.next().ok_or("unterminated string literal")?;
        match ch {
            '"' => break,
            '\\' => {
                let esc = chars.next().ok_or("unterminated string literal")?;
                lit.push(match esc {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '"' => '"',
                    '\\' => '\\',
                    '#' => '#',
                    other => other,
                });
            }
            '#' if chars.peek() == Some(&'{') => {
                chars.next();
                if !lit.is_empty() {
                    parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                }
                let mut depth = 1;
                let mut expr = String::new();
                loop {
                    let ch = chars
                        .next()
                        .ok_or("unterminated string interpolation")?;
                    match ch {
                        '{' => {
                            depth += 1;
                            expr.push('{');
                        }
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            expr.push('}');
                        }
                        other => expr.push(other),
                    }
                }
                parts.push(StrPart::Interp(expr));
            }
            other => lit.push(other),
        }
    }
    if !lit.is_empty() || parts.is_empty() {
        parts.push(StrPart::Lit(lit));
    }
    tokens.push(Token::Str(parts));
}
```

ポイント: `\#` はエスケープ処理アームで消費されるため `\#{...}` は補間にならない(新機能)。`ok_or` を使うため既存の `match chars.next() { None => return Err(...) }` パターンより簡潔。

(この時点では parser がコンパイルエラーになる — 次タスクで直すまでコミットしない)

### Task 6.2: AST と parser に補間を実装し、runtime から re-parse を削除

**Files:**
- Modify: `src/ast.rs`, `src/parser.rs`, `src/runtime.rs`
- Test: `tests/in_memory.rs`

- [ ] **Step 1: ast.rs に追加**

```rust
pub enum Expr {
    Int(i64),
    Str(String),
    StrInterp(Vec<StrPiece>),
    // ... 既存の variant は不変
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPiece {
    Lit(String),
    Expr(Expr),
}
```

- [ ] **Step 2: parser.rs を更新**

`parse_primary` の `Token::Str(s)` アーム:

```rust
Token::Str(parts) => {
    self.bump();
    Self::expr_from_string_parts(parts)
}
```

新規関数(impl Parser 内, associated fn):

```rust
fn expr_from_string_parts(parts: Vec<StrPart>) -> Result<Expr, Box<dyn Error>> {
    if let [StrPart::Lit(s)] = parts.as_slice() {
        return Ok(Expr::Str(s.clone()));
    }
    let mut pieces = vec![];
    for part in parts {
        match part {
            StrPart::Lit(s) => pieces.push(StrPiece::Lit(s)),
            StrPart::Interp(src) => pieces.push(StrPiece::Expr(Self::parse_embedded_expr(&src)?)),
        }
    }
    Ok(Expr::StrInterp(pieces))
}

fn parse_embedded_expr(src: &str) -> Result<Expr, Box<dyn Error>> {
    let tokens = crate::lexer::lex(src)?;
    let program = Parser::new(tokens).parse_program()?;
    match program.as_slice() {
        [Stmt::Expr(e)] => Ok(e.clone()),
        _ => Err("interpolation must contain a single expression".into()),
    }
}
```

`parse_map_literal` のキー処理(補間キーを明示エラーに):

```rust
let key = match self.peek().clone() {
    Token::Str(parts) => {
        self.bump();
        match parts.as_slice() {
            [StrPart::Lit(s)] => s.clone(),
            _ => return Err("interpolation is not allowed in map keys".into()),
        }
    }
    Token::Ident(s) => {
        self.bump();
        s
    }
    t => {
        return Err(format!("object key must be string or identifier, got {t:?}").into())
    }
};
```

use 追加: `use crate::ast::{BinOp, Expr, Stmt, StrPiece};` / `use crate::lexer::{StrPart, Token};`

- [ ] **Step 3: runtime.rs を更新**

- `Expr::Str(s)` の評価を `Ok(Value::Str(s.clone()))` に戻す(補間スキャン削除)
- `Expr::StrInterp` の評価を追加:

```rust
Expr::StrInterp(pieces) => {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            StrPiece::Lit(s) => out.push_str(s),
            StrPiece::Expr(e) => {
                let v = self.eval_expr(e)?;
                out.push_str(&v.to_string());
            }
        }
    }
    Ok(Value::Str(out))
}
```

- `eval_interpolated_string` と `eval_embedded_expr` を削除
- `use crate::{lexer, parser};` を削除(runtime → lexer/parser 依存の解消)

- [ ] **Step 4: 新挙動のテストを書く**(`tests/in_memory.rs` に追記)

```rust
#[test]
fn escaped_interpolation_stays_literal() {
    let out = run_script("\"a \\#{1} b\"").unwrap();
    assert_eq!(out, Value::Str("a #{1} b".to_string()));
}

#[test]
fn interpolation_in_map_key_is_rejected() {
    let err = run_script("{\"k#{1}\": 1}").unwrap_err();
    assert!(
        format!("{err}").contains("interpolation is not allowed in map keys"),
        "{err}"
    );
}

#[test]
fn unterminated_interpolation_fails_at_lex_time() {
    let err = firelang::lexer::lex("\"#{1 + 2\"").unwrap_err();
    assert!(format!("{err}").contains("unterminated"), "{err}");
}
```

- [ ] **Step 5: 検証、spec 更新、コミット**

Run: `cargo test --all-targets && cargo clippy --all-targets -- -D warnings`
Expected: 全 green。既存の `supports_ruby_style_string_interpolation` と examples/03 のテストが green のままであること

`docs/spec.md` 4 章を更新: `\#{` エスケープ可、map キーでの補間はエラー、13 章の該当項(補間まわり 2 件)を削除。

```bash
git add src/ tests/in_memory.rs docs/spec.md
git commit -m "refactor: resolve string interpolation at parse time; support \\#{ escape"
```

---

## Phase 7: 仕上げ

### Task 7.1: examples/09 を実際の emulator スモークにする

01 と同一内容(問題 #15)なので、両バックエンドで動く set/get 往復に差し替える。

**Files:**
- Modify: `examples/scripts/09_emulator_smoke.fl`
- Modify: `tests/example_scripts.rs`

- [ ] **Step 1: スクリプトを差し替える**

```ruby
# Emulator smoke: run with
#   FIRESTORE_EMULATOR_HOST=... FIRESTORE_PROJECT_ID=... fl --backend emulator examples/scripts/09_emulator_smoke.fl
set("smoke/fl", {ok: true, checked_at: server_timestamp()})
get("smoke/fl")
```

- [ ] **Step 2: tests/example_scripts.rs を更新**

`cases` の vec から `("examples/scripts/09_emulator_smoke.fl", Value::Int(5))` の行を削除し、shape テストを追加:

```rust
#[test]
fn emulator_smoke_example_round_trips_in_memory() {
    let src = std::fs::read_to_string("examples/scripts/09_emulator_smoke.fl").unwrap();
    let out = run_script(&src).unwrap();
    let s = format!("{out:?}");
    assert!(s.contains("Bool(true)") && s.contains("Timestamp"), "{s}");
}
```

- [ ] **Step 3: 検証してコミット**

Run: `cargo test --test example_scripts`
Expected: 4 passed

```bash
git add examples/scripts/09_emulator_smoke.fl tests/example_scripts.rs
git commit -m "chore: make emulator smoke example exercise set/get round-trip"
```

### Task 7.2: 最終整合チェック

- [ ] **Step 1: spec と実装の突き合わせ**

`docs/spec.md` の「既知の制限・実装メモ」(13 章)を読み、Phase 2-6 で解消済みの項目がすべて削除されていることを確認。残っているべき項目: パス検証なし / emulator abort のロールバック RPC なし / Debug 出力形式。

- [ ] **Step 2: フル検証**

Run: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`
Expected: すべて成功

Run: `firebase emulators:exec --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'`
Expected: 10 passed

- [ ] **Step 3: 行数確認(成果の定量化)**

Run: `wc -l src/*.rs src/firestore/*.rs`
Expected 目安: runtime.rs ≈ 600 行(現 1147)、firestore/memory.rs ≈ 250、firestore/emulator.rs ≈ 850(現 957)、value.rs ≈ 200。トータルで現状比 300-400 行減

- [ ] **Step 4: コミット(残差分があれば)**

```bash
git add -A && git commit -m "docs: final spec/implementation sync after refactoring"
```

---

## Self-Review チェックリスト(プラン作成時に実施済み)

- 発見した問題 #1-#15 すべてにフェーズ/タスクが割り当てられている(#1→P1 済、#2-#5→P2、#6-#7→P3、#8-#9→P4、#10-#12→P5、#13-#14→P6、#15→P7)
- 後続タスクが前タスク定義の名前を使う箇所の整合: `to_query()`(4.3 Step 5 で定義 → 5.1 で使用)、`run_query`/`transaction_get`/`apply`(4.1 定義 → 4.2/4.3 使用)、`ParsedArgs`(2.4 定義 → 5.3 使用)、`StrPart`/`StrPiece`(6.1/6.2)、`Value::to_string()`(3.1 の Display impl → 6.2 で使用)
- 挙動変更はすべて意図的かつ spec 更新とセット: ネストエラー文言(4.3)、limit/where の batch/tx 内許可(5.1)、未知 where 演算子の即時エラー(5.2)、`\#{` エスケープと map キー補間エラー(6.2)、RFC3339 小数秒受理(2.2)
- 各フェーズ末で `cargo test --all-targets` green、Phase 4 と最終で emulator 実機テスト必須
