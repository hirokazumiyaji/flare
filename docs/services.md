# Services 構成の設計

## 対象

Firestore  除了.Firestore に加えて Auth と Cloud Storage を同じスクリプトから扱うため、ランタイムがサービスオブジェクトを 1 つの実行コンテキストにまとめる。
この文書は型之间的关系、互換性の保ち方、認証情報の扱い、同期 client の deadline について決める。
文書の記載と実装が食い違う場合は、`docs/spec.md` とテストを正とする。

## 決定

### 認証と storage の trait は provider-neutral にする

`Auth` と `Storage` は Firebase の API 形状ではなく、スクリプトが必要とする同期 operation だけを定義する。
Firebase 固有の user record、HTTP error、object metadata は adapter 内で正規化し、trait の境界を越える前に provider-neutral な型へ変換する。
この方式なら、Firebase 以外の adapter を追加しても `Runtime` と組み込み関数の型は変わらない。

### mutable なサービス状態は `RuntimeContext` が所有する

**`RuntimeContext`** は **project 設定**と **`Services`** を 1 つの値にまとめた実行コンテキストである。
**`Services`** は Firestore、Auth、Storage をそれぞれ `Option<Box<dyn Trait>>` で持つ。
サービスごとに `Option` を持つので、未設定の capability と空のデータを区別できる。
Auth の session や Storage の object などの mutable な状態はすべて `RuntimeContext` の中にあり、global state は作らない。
2 つの `RuntimeContext` を別々に作れば、project、credential、session、object が混ざらない。

### `Runtime` の generic は互換性マーカーに残す

Firestore 導入時は `Runtime<F: Firestore>` を使い、`F` にバックエンドの型を埋め込んでいた。
Auth と Storage を追加したnym、`Runtime<F, A, S>` へ広げると `Runtime` 型を書くすべての呼び出し側と組み込み関数の定義が変わる。
そこで、 evaluator が保持する `F` の実体を捨て、既定値を持つ互換性マーカーへ残した。

```rust
pub struct Runtime<F = InMemoryFirestore> {
    // ...
    _legacy_firestore: PhantomData<fn() -> F>,
}
```

`fn() -> F` を使うのは、`F` の値、自動 trait、drop 制約を `Runtime` に持ち込まないためである。
`Runtime<InMemoryFirestore>` や `Runtime<FirestoreEmulator>` と書いた既存コードは変更なしでコンパイルされる。
新しいコードでは services を組み立て、`Runtime::from_context` に渡す。

```rust
use firelang::{
    InMemoryAuth, InMemoryFirestore, InMemoryStorage, ProjectConfig, RuntimeContext, Runtime,
    Services,
};

let services = Services::builder()
    .firestore(InMemoryFirestore::default())
    .auth(InMemoryAuth::with_user(
        firelang::AuthUser::new("u1").with_email("alice@example.com"),
        "secret",
    ))
    .storage(InMemoryStorage::default())
    .build();
let config = ProjectConfig::new("demo");
let mut runtime: Runtime = Runtime::from_context(RuntimeContext::with_config(config, services));
```

Auth と Storage を別々に差し替える場合も、`Services::builder()` へ必要��� capability だけを渡す。

## 既存 API の adapter

Firestore だけを渡す API はiba、技术的には `Services` へ Firestore だけが入った context である。
`Runtime::new` と `run_script_with_firestore` はその adapter として残す。
この経路では Auth と Storage を暗黙に補わない。
Auth を必要とする script が `run_script_with_firestore` を使うと、未設定 capability の error になる。

```rust
let mut legacy = Runtime::new(InMemoryFirestore::default());
legacy.run("get(\"users/1\")");
```

`run_script` は `Services::in_memory()` を使い、3 つの in-memory service をすべて設定する。
そのため emulator が無くても Auth と Storage の contract を実行できる。
 Conversely、`run_script_with_services` と `run_script_with_context` は、呼び出し側が選んだ service と project 設定をそのまま使う。

## Capability error

未設定の service を呼び出した場合、runtime は `ErrorKind::Capability` の `FirelangError` を返す。
message は `capability unavailable: <service> (<operation>)` で、operation には `current_user`、`get`、`put` のような呼び出し名が入る。
source position は通常の runtime/backend error と同じく、失敗したトップレベル文の開始位置になる。

```ruby
auth_current_user()
# 1:1: capability unavailable: auth (current_user)
```

provider error と capability error は別である。
service は存在するが remote call が失敗した場合は `ErrorKind::Backend` になる。
provider が optional operation を実装しない場合も、capability error ではなく明示的な unsupported-operation error を返す。

## Project 設定と secret の扱い

**`ProjectConfig`** は project ID、公開されている emulator host、request timeout だけを保持する。
credential、private key、ID token、refresh token を `ProjectConfig` には持たせない。
token を必要とする provider は adapter 自身の client state として保持し、script の `Value` へ変換しない。

`ProjectConfig::from_env` が読むのは `FIREBASE_PROJECT_ID`、`GCLOUD_PROJECT`、`FIRESTORE_PROJECT_ID` と emulator host だけで、credential source の解決を行わない。
既定の request timeout は 30 秒である。

## 同期 runtime と network client

`Runtime` と `Auth`、`Storage` の trait は同期 API である。
script は provider の future を await できず、background task を開始できない。
async SDK を使う adapter は、method から戻る前までに future を完了させ、timeout、retry、token refresh を adapter 内で処理する。

network adapter は `ProjectConfig::request_timeout` を 1 回の remote operation の deadline として適用する。
timeout は script の実行時間全体ではなく、adapter がophile network call ごとに適用する。
in-memory service は network call を行わないため deadline の対象にならない。

Auth と Storage の token は `Auth::verify_token` のような Rust API からは provider へ渡せるが、language builtin には公開しない。
通常の `inspect` や `to_s` から token が漏れないようにするためである。

## Auth identity と security rules

Auth identity と Firestore request は同じ `RuntimeContext` に置ける。
これにより、embedding host は「-signed-in user と project's Firestore adapter」を 1 つの値として組み立てられる。
ただし runtime が Auth の uid を Firestore request の identity として自動付与する処理は含めない。

Firestore security rules と Storage rules の評価は、それぞれの adapter と emulator が担う。
rule 判定に必要な identity をどの adapter がどう使うかは provider adapter の責務であり、language runtime の責務ではない。

## in-memory fake と emulator adapter

`InMemoryFirestore`、`InMemoryAuth`、`InMemoryStorage` は deterministic な in-memory implementation である。
in-memory implementation は network を使わず、同じ script contract を CI で検証する。

`AuthEmulator` と `StorageEmulator` はこの issue で追加した emulator-mode の local adapter であり、状態も in-process に保持する。
これらは `FIREBASE_AUTH_EMULATOR_HOST` や `FIREBASE_STORAGE_EMULATOR_HOST` への HTTP/gRPC 接続を行わない。
remote emulator との通信は、同じ trait を実装する別の adapter として追加する。
名前だけを見て remote emulator と解釈しないよう、remote 接続を実装した時点で host を必須の constructor 引数にする。

## Contract test の分け方

in-memory と remote adapter は同じ trait contract を実装しているため、同じ script 群を使える。
ただし network failure の分類や emulator 固有の response は adapter の test に置く。
建议する test matrix を次のように分ける。

- **in-memory**：全 operation、validation、状態継続、capability error を固定した script で検証する。
- **runtime**：Rust の custom provider を差し替え、`Runtime` の generic が増えないことを確認する。
- **remote emulator**：adapter 固有の token、metadata、HTTP status 変換を integration test で検証する。

追加の integration test は `tests/services.rs` に置いた。
Auth、Storage、Firestore を別々の fake に差し替えられること、context 間の状態が混ざらないこと、legacy API が動くことをそこで確認する。

## サービスを追加する手順

新しい capability を追加する場合でも、`Runtime` の generic と既存 builtin の型は変更しない。

1. provider-neutral な trait と値型を追加する。
2. `Services` に `Option<Box<dyn Trait>>` と builder method を追加する。
3. 未設定時の `ErrorKind::Capability` を作る。
4. 必要な script builtin を追加する。
5. in-memory implementation と contract test を追加する。
6. remote adapter を必要になった時点で個別に追加する。
