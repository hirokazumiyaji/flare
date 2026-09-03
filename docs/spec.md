# Firelang 言語仕様 / API リファレンス

このドキュメントが firelang の**唯一の正式仕様(authoritative spec)**です。README・サンプル・テストとこの仕様が食い違う場合は、この仕様を正として実装またはドキュメントを直してください。挙動を変更するコミットでは、この仕様を同じコミットで更新します。

対象バージョン: 0.1.0(MVP)

- [1. 概要](#1-概要)
- [2. スクリプトの構造](#2-スクリプトの構造)
- [3. 値と型](#3-値と型)
- [4. リテラルと式](#4-リテラルと式)
- [5. 変数・関数・制御構文](#5-変数関数制御構文)
- [6. Firestore API](#6-firestore-api)
- [7. クエリ](#7-クエリ)
- [8. batch / transaction](#8-batch--transaction)
- [9. Timestamp API](#9-timestamp-api)
- [10. CLI (`fl`)](#10-cli-fl)
- [11. バックエンド](#11-バックエンド)
- [12. スコープ外(MVP)](#12-スコープ外mvp)
- [13. 既知の制限・実装メモ](#13-既知の制限実装メモ)

## 1. 概要

firelang は Firestore を操作するための mruby 風スクリプト言語です。mruby 完全互換ではなく、Firestore 操作に必要な軽量サブセットのみを提供します。

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"
```

## 2. スクリプトの構造

- ソースは UTF-8 テキスト。文(statement)の区切りは改行または `;`。
- `#` から行末まではコメント(文字列リテラル内を除く)。
- プログラムの評価結果は、最後に評価された文の値。`return` がトップレベルで実行された場合はその値。
- `fl` CLI は評価結果を `=> <Debug表現>` 形式で標準出力に印字する。

## 3. 値と型

| 型 | 例 | 説明 |
|----|----|------|
| Int | `42` | 64bit 符号付き整数。**数値は整数のみ**(Float は非対応) |
| Str | `"hello"` | UTF-8 文字列 |
| Bool | `true` / `false` | 真偽値 |
| Nil | `nil` | 値なし。存在しないドキュメントの `get` も nil |
| Array | `[1, 2, "a"]` | 任意の値の列 |
| Map | `{name: "a", age: 1}` | 文字列キーの辞書。キー昇順で保持される |
| Timestamp | `timestamp(100)` / `now()` | Unix 秒(整数)。ナノ秒精度なし |
| Func | `fn f(a) { a }` | ユーザー定義関数 |

Firestore 操作のための特殊な値:

| 型 | 生成方法 | 説明 |
|----|---------|------|
| DocRef | `doc("users/1")` | ドキュメント参照 |
| CollectionRef | `col("users")` | コレクション参照 |
| CollectionGroupRef | `col_group("members")` | コレクショングループ参照 |
| QueryRef | `col(..).where(..)` 等 | 構築中のクエリ |
| ServerTimestamp | `server_timestamp()` | 書き込み時にサーバ時刻へ解決されるセンチネル |
| ArrayUnion / ArrayRemove | `array_union([..])` / `array_remove([..])` | 配列フィールド変換センチネル |

センチネル 3 種(ServerTimestamp / ArrayUnion / ArrayRemove)は `set` / `update` の data_map の**直下のフィールド値**としてのみ意味を持つ。ネストした Map / Array の中では使えない(emulator バックエンドではエラー)。

### 真偽値判定(truthiness)

`false` と `nil` のみ偽。それ以外はすべて真(`0` も `""` も `[]` も真)。

## 4. リテラルと式

### 整数リテラル

10 進のみ。**負のリテラルはない**(単項マイナス非対応)。負値は `0 - 5` のように書く。

### 文字列リテラル

ダブルクォートのみ。エスケープ: `\n` `\r` `\t` `\"` `\\`(その他の `\x` は `x` がそのまま残る)。

文字列補間 `#{式}` をサポートする:

```ruby
name = "fire"
"hello #{name} #{1 + 2}"  # => "hello fire 3"
```

- `#{}` の中身は**単一の式**でなければならない(文・複数式はエラー)。
- `\#{` で `#{` をリテラルとして書ける(エスケープ)。
- マップリテラルの**キー**内の補間(`{"k#{1}": v}`)はエラー。

### 配列・マップリテラル

```ruby
[1, 2, "a"]
{name: "alice", "with space": 1, nested: {a: [1]}}
```

マップのキーは識別子または文字列リテラル。値は任意の式。

### 要素アクセス(`expr[expr]` と `m.key`)

Map と Array は添字アクセスできる。メソッド呼び出しと同様の postfix として連結でき、`get()` 結果などの任意の式に対して使える:

```ruby
m = {name: "alice", nested: {a: [1, 2]}}
m["name"]          # => "alice"
m["nested"]["a"][1]  # => 2
m.name             # => "alice"(Map に限り m["name"] と同等の糖衣構文)

rows = col("users").get()
rows[0]["__name__"]  # => "users/1"(クエリ結果 Map の要素アクセス。__name__ も同じ経路で取れる)
```

- Map への添字は **Str のみ**。Array への添字は **Int のみ**。型が合わなければエラー。
- 存在しないキー・範囲外のインデックスは **nil** を返す(Ruby 準拠)。負のインデックスも nil。
- Map / Array 以外のレシーバへの添字アクセスはエラー(`unsupported index receiver`)。
- `m.key` 形式は Map レシーバ限定(Ref / Query 型の `.doc(..)` / `.where(..)` などとの衝突を避けるため)。存在しないキーは nil。引数付きの呼び出し(`m.key(...)`)はエラー。

### 演算子と優先順位(低い順)

| 優先順位 | 演算子 | 対応型 |
|----------|--------|--------|
| 1(最低) | `==` `!=` | 任意の値同士(構造比較) |
| 1 | `>` `<` `>=` `<=` | Int 同士のみ。他はエラー |
| 2 | `+` `-` | `+`: Int 同士 / Str 同士(連結)。`-`: Int のみ |
| 3 | `*` `/` | Int のみ。`/ 0` は "division by zero" エラー |
| 4(最高) | `.method(...)` / `expr[expr]` | メソッド呼び出し・要素アクセス(§4 要素アクセス参照) |

論理演算子(`&&` `||` `!`)はない。括弧 `( )` でグループ化できる。

## 5. 変数・関数・制御構文

### 変数(immutable)

```ruby
a = 1
a = 2   # エラー: reassignment is not allowed: a
```

- 変数は再代入不可。関数定義名も同じ名前空間を使い、再定義不可。
- 関数本体内でも外側スコープに見える名前への代入はエラー(シャドーイング不可)。ただし**関数の仮引数**は外側の名前と重なってもよい。

### 関数定義と呼び出し

```ruby
fn add(a, b) { return a + b }   # return は省略可(最後の式が戻り値)
add(2, 3)
add(a: 2, b: 3)                 # Ruby 風キーワード引数
puts "x"                        # 括弧なし呼び出しも可
```

- 引数は positional とキーワードを混在できる。positional が先に埋まり、残りはキーワード名で束縛。過不足・未知キーワードはエラー。
- **メソッド呼び出し(レシーバあり)ではキーワード引数は使えない。**
- `fn name { ... }` のように引数リスト自体を省略できる。

### メソッドと不変性

オブジェクトのメソッドは変更後の**コピーを返す**。レシーバは変化しない。

```ruby
t1 = timestamp(100)
t2 = t1.add_seconds(10)   # t1 は 100 のまま
```

### 制御構文

```ruby
if cond { ... } else { ... }   # else 省略可
while cond { ... }
```

ブロックは常に `{ ... }`。`if` / `while` も式として最後に評価した値を返す(`while` が一度も回らなければ nil)。

### 組み込み出力

`puts(v, ...)` — 各値を 1 行ずつ印字して nil を返す。スカラー以外(Array / Map 等)は Debug 形式で印字される。

## 6. Firestore API

`firestore` オブジェクトは存在しない。すべて**グローバル関数**として提供する。

ドキュメントパスを取る引数(下記 `target`)には、パス文字列(`"users/1"`)または `doc(...)` の戻り値のどちらでも渡せる。

| 関数 | 戻り値 | 説明 |
|------|--------|------|
| `doc(path)` | DocRef | ドキュメント参照を作る |
| `col(path)` | CollectionRef | コレクション参照。`"users"` や `"teams/t1/members"`(サブコレクション)が使える |
| `col_group(id)` | CollectionGroupRef | 同名コレクション横断の参照 |
| `get(target)` | Map / Nil | ドキュメント取得。**存在しなければ nil**(エラーにはならない) |
| `set(target, data_map)` | Nil | ドキュメント全体を書き込み(置換) |
| `set(target, data_map, merge_bool)` / `set(target, data_map, merge: true)` | Nil | merge=true ならフィールドマージ |
| `update(target, data_map)` | Nil | フィールド更新。**ドキュメントが存在しなければエラー**(作成しない) |
| `delete(target)` | Nil | ドキュメント削除。存在しなくてもエラーにならない |
| `array(...)` | Array | `[...]` リテラルと同等 |
| `array_union(array)` | センチネル | 配列フィールドへの追加(重複は追加されない) |
| `array_remove(array)` | センチネル | 配列フィールドからの除去 |
| `server_timestamp()` | センチネル | 書き込み時にサーバ時刻に解決 |
| `batch { ... }` | ブロックの値 | 8 章参照 |
| `transaction { ... }` | ブロックの値 | 8 章参照 |

`data_map` の値に使えるのは: Int / Str / Bool / Nil / Array / Map / Timestamp、および直下フィールドのセンチネル 3 種。

```ruby
set(doc("users/1"), {name: "alice", age: 20})
set("users/1", {age: 21}, true)                       # merge
update("users/1", {tags: array_union(["a"])})
```

## 7. クエリ

```ruby
col("users").where("age", ">=", 20).where("active", "==", true).limit(10).get()
col_group("members").where("active", "==", true).get()
col("users").get()          # 直下の全ドキュメント
col("users").limit(10).get()
col("users").doc("1")       # doc("users/1") と同等
```

- `where(field, op, value)` — op は `"=="` `">"` `">="` `"<"` `"<="` の 5 種。複数 `where` は AND。
  - 未対応の op を渡すと `where()` 呼び出し時に即エラー(`unsupported query op: ...`)。
  - `==` は任意の値の構造比較。大小比較(`>` 等)は **Int フィールドのみ**マッチし、他の型は偽。
  - フィールドが存在しないドキュメントはマッチしない。
- `limit(n)` — 件数上限。後勝ち(複数回呼ぶと最後の値)。
- `get()` — 実行して `Array<Map>` を返す。各 Map には `__name__` キーで**相対ドキュメントパス**(例 `"users/1"`)が入る。
- **決定的順序**: 結果は firelang 相対ドキュメントパス文字列の辞書順でソートされ、`limit` はソート後に適用される。`users/a` < `users/b`、`teams/t1/members/a` < `teams/t2/members/a`。
- `col()` のクエリは**直下のドキュメントのみ**対象(サブコレクション内は含まない)。`col_group(id)` はパス中のいずれかの階層でコレクション名が `id` に一致するドキュメントすべてが対象。
- クエリの**構築**(`.where()` / `.limit()`)は `batch` / `transaction` 内でも可。**実行**(`.get()`)のみ不可。
- カーソル・オフセット・`order_by` は非対応(12 章)。

## 8. batch / transaction

どちらも `{ ... }` ブロックが必須で、ブロックの最後の値を返す。ネスト(batch/transaction 内での batch/transaction)は不可(エラーメッセージ: `batch cannot be nested inside batch/transaction` / `transaction cannot be nested inside batch/transaction`)。

### batch

- **write 専用**: `set` / `update` / `delete` をステージする。`get` とクエリ実行はエラー。
- ブロックが成功したら全 write を**原子的に**適用する。
- ランタイムエラーでもバックエンドエラーでも、失敗時は **1 件も書き込まれない**(部分書き込みなし)。

### transaction

- 使えるのは `get` / `set` / `update` / `delete`。
- `get` は**最初の write をステージする前**なら何度でも呼べる(同一ドキュメントの再読・複数ドキュメント可)。読み取りはトランザクション開始時点のスナップショット(emulator ではトランザクション ID 付き読み取り)。
- **最初の write 以降の `get` はエラー**("get is not allowed after the first write in a transaction")。
- クエリ実行は不可。
- **単一試行**: 自動リトライしない。
- 失敗時(ランタイム/バックエンドいずれも)は 1 件も書き込まれない。

```ruby
transaction {
  v = get("users/1")
  set("users/1", {visits: 1})
}
```

## 9. Timestamp API

| 式 | 説明 |
|----|------|
| `now()` | 現在時刻の Timestamp(Unix 秒) |
| `timestamp(1700000000)` | Unix 秒から生成 |
| `timestamp("2026-02-01T00:00:00+09:00")` | RFC3339 文字列から生成。オフセットは `Z` または `±HH:MM`。小数秒は非対応 |
| `ts.to_unix()` | Int(Unix 秒)を返す |
| `ts.add_seconds(n)` | n 秒後の**新しい** Timestamp を返す |

## 10. CLI (`fl`)

```
usage: fl [--backend in-memory|emulator] <script-file>
```

- スクリプトファイルはちょうど 1 つ。`--help` / `-h` で usage 表示。未知のオプション・未知のバックエンドはエラー終了。
- `--backend emulator` のとき:
  - `FIRESTORE_EMULATOR_HOST` — **必須**。未設定なら即エラー。
  - `FIRESTORE_PROJECT_ID` — 省略時 `firelang-test`。
- 実行結果は `=> <Debug表現>` で印字。

## 11. バックエンド

| エントリポイント | `in-memory` | `emulator` |
|------------------|-------------|------------|
| `run_script` | ○(固定) | × |
| `run_script_with_firestore` | ○ | ○(`FirestoreEmulator` を渡す) |
| `fl` CLI | ○(デフォルト) | ○(`--backend emulator`) |

- 認証付きの**本番 Firestore は非対応**(スコープ外)。
- ランタイムは `Firestore` トレイトでバックエンドを抽象化する。デフォルトは `InMemoryFirestore`。
- emulator バックエンドは Firestore Rust SDK(`firestore` crate)の gRPC クライアントで Firestore API を呼ぶ。database は `(default)` 固定。
- 両バックエンドは同じ可視挙動(値の往復・クエリ順序・batch/transaction の原子性)を持たなければならない。

## 12. スコープ外(MVP)

以下は明示的に**非対応**。実装しない・README で匂わせない。

- Float(数値は Int のみ)
- 本番 Firestore への認証付きアクセス
- transaction の自動リトライ
- `limit` 以外のページネーション(カーソル・オフセット)
- `order_by`・リスナー(リアルタイム購読)・インデックス依存機能
- ドキュメントに書かれたサブセットを超える mruby 互換

## 13. 既知の制限・実装メモ

実装済みコードから確認された、仕様としては保証しない・将来直す予定の挙動。

| 項目 | 現状の挙動 | 予定 |
|------|-----------|------|



| ドキュメントパスの検証 | セグメント数の検証なし(`set("users", ...)` が通る)。挙動未定義 | backlog(当面そのまま) |
| emulator の transaction 中断 | ロールバック RPC を送らない(サーバ側タイムアウト任せ) | backlog |
| `puts` / `fl` の複合値出力 | Rust の Debug 形式(`Map({"a": Int(1)})`) | backlog |
