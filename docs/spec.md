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
- マップリテラル `{ ... }`、配列リテラル `[ ... ]`、引数リスト `( ... )`、関数・ブロックの仮引数リスト、グループ化 `( expr )`、添字 `[ ... ]` の**内側では改行を無視する**(Ruby と同じ)。要素・値・引数となる**式の途中**の改行も無視される。したがってリテラル・引数リストは複数行に書ける。**末尾カンマ**(`{a: 1,}` / `[1, 2,]` / `f(a, b,)`)も許容する:
  ```ruby
  set("users/1", {
    name: "a",
    tags: [
      "x",
      "y",
    ],
  })
  [1 +
    2]   # 式の途中の改行も可
  ```
  ただし `fn` / `if` / `while` / ブロック付きメソッドの `{ ... }`(文の並び)では改行は文の区切りのまま。delimiter の内側では `;` は改行と同じにはならない(要素の区切りは `,` のみ。`[;1]` 等はエラー)。
- `#` から行末まではコメント(文字列リテラル内を除く)。
- プログラムの評価結果は、最後に評価された文の値。`return` がトップレベルで実行された場合はその値。
- `fl` CLI は評価結果を `=> <inspect表現>` 形式で標準出力に印字する。

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
| DeleteField | `delete_field()` | フィールド削除センチネル(`update` / merge `set` のみ) |
| Increment | `increment(n)` | 数値加算センチネル |
| Opaque | (生成不可。emulator 読み取りのみ) | `double` / `geo_point` / `bytes` フィールドの表示専用値(`Double(1.5)` 等)。書き込みには使えない |

センチネル 5 種(ServerTimestamp / ArrayUnion / ArrayRemove / DeleteField / Increment)は `set` / `update` の data_map の**直下のフィールド値**としてのみ意味を持つ。ネストした Map / Array の中では使えない(emulator バックエンドではエラー)。

emulator バックエンドで firelang が持たない型のフィールドを含むドキュメントを読んだ場合、ドキュメント全体はエラーにせず次のように読み替える:`double` / `geo_point` / `bytes` は `Opaque` の表示専用値(例 `Double(1.5)`、`GeoPoint(35.7, 139.7)`、`Bytes(00ff)`)、`reference` は同一データベース内であれば相対パスの `DocRef`(別プロジェクトや別データベースの参照は `Reference(...)` の `Opaque` 値)。`Opaque` 値はクエリの `==` 等でマッチさせることはできず(スクリプトからは生成できない)、書き戻しはエラー(`unsupported value in emulator write`)。

### 真偽値判定(truthiness)

`false` と `nil` のみ偽。それ以外はすべて真(`0` も `""` も `[]` も真)。

## 4. リテラルと式

### 整数リテラル

10 進のみ。負値は単項マイナスで書く(`-5`、`-(a + b)`)。i64 の範囲を超えるリテラルは lexer でエラー(`integer literal out of range`)。`-9223372036854775808`(`i64::MIN`)は正の部分が範囲外のためリテラルとしては書けず、`0 - 9223372036854775807 - 1` のように書く。単項マイナスの結果が範囲外(例: `-` を `i64::MIN` に適用)になると `integer overflow` エラー。

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
| 1(最低) | `\|\|` | 任意の値(Ruby と同じく短絡評価し、オペランドそのものを返す) |
| 2 | `&&` | 任意の値(短絡評価し、オペランドそのものを返す) |
| 3 | `==` `!=` | 任意の値同士(構造比較) |
| 3 | `>` `<` `>=` `<=` | Int 同士 / Timestamp 同士のみ。他はエラー |
| 4 | `+` `-` | `+`: Int 同士 / Str 同士(連結)。`-`: Int 同士 / Timestamp − Timestamp → Int(秒差)。他はエラー |
| 5 | `*` `/` | Int のみ。`/ 0` は "division by zero" エラー |
| 6 | `!` `-` (単項) | `!v` は任意の値を受け `Bool(!truthy(v))`。`-v` は Int のみ(範囲外は `integer overflow`) |
| 7(最高) | `.method(...)` / `expr[expr]` | メソッド呼び出し・要素アクセス(§4 要素アクセス参照) |

- `&&` は左辺が偽(`false` / `nil`)なら左辺を返し、そうでなければ右辺を返す(右辺は評価しない)。
- `||` は左辺が真なら左辺を返し、そうでなければ右辺を返す(右辺は評価しない)。
- `!` / `-` (単項)は右結合で、`!!x` は `!(!x)`、`--5` は `-(-5)` と解釈される。`!=` との区別は lexer が行う(`a != b` は比較、`!a` は否定)。
- 真偽値判定は §3 の truthiness に従う(`false` と `nil` のみ偽)。
- 括弧なし呼び出しの最初の引数に単項マイナスは書けない。`puts -1` は `puts - 1`(減算)と解釈されるため、負値を渡すときは括弧を付ける(`puts(-1)`)。`!` は書ける(`puts !x` は `puts(!x)`)。

Int 同士の算術演算(`+` `-` `*` `/`)の結果が i64 の範囲を超えると `integer overflow` エラー(debug ビルドでも panic しない)。`/ 0` は "division by zero" エラー。

## 5. 変数・関数・制御構文

### 変数(immutable)

```ruby
a = 1
a = 2   # エラー: reassignment is not allowed: a
```

- 変数は再代入不可。関数定義名も同じ名前空間を使い、再定義不可。
- 関数本体内でも外側スコープに見える名前への代入はエラー(シャドーイング不可)。ただし**関数の仮引数**は外側の名前と重なってもよい。

### スコープ規則(静的スコープ)

関数呼び出し時に見えるのは**グローバルスコープと仮引数のみ**。呼び出し元のローカル変数や、呼び出し先で定義されたローカルは関数から見えない(Ruby と同じ静的スコープ):

```ruby
fn f() { x }
fn g() { x = 1
  f() }
g()      # エラー: undefined variable: x
```

- 関数からグローバル変数は読める。再帰呼び出し(グローバルに定義した関数の自己参照)も可能。
- 関数本体内の代入は「見える名前」(グローバル・仮引数・関数内ローカル)への再定義はエラー。**呼び出し元にしか存在しない名前**なら、関数内ローカルとして定義できる。
- ブロック(`each` / `map` 等)の仮引数も上記と同じ規則で外側の名前と重複してよい。ブロック本体内からは外側スコープの変数が読める(ブロックは呼び出し元の環境で評価される)。

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
- 括弧なし呼び出しの**最初の引数に配列リテラルは書けない**。`puts [1, 2]` は `puts` への添字アクセス(`puts[1, 2]`)と解釈されるため、配列を渡すときは括弧を付ける(`puts([1, 2])`)。

### ブロック付きメソッド呼び出しと反復

メソッド呼び出しには `|params|` を持つブロックを渡せる。ブロック引数はブロック内の新しいスコープに束縛され、外側の変数と同名でもよい(仮引数と同じ規則)。変数は再代入不可のため、反復結果の蓄積は `map` で行う:

```ruby
rows = col("users").where("active", "==", true).get()
rows.each { |row| update(row["__name__"], {seen: true}) }   # 各要素に副作用。戻り値は nil
doubled = [1, 2, 3].map { |n| n * 2 }                       # 各要素を変換した新しい Array
total = len(rows)                                            # 要素数
```

| 式 | 説明 |
|----|------|
| `array.each { \|item\| ... }` | 各要素をブロック引数に束縛して評価。戻り値は nil。**クエリ結果(`.get()` の戻り値)にもそのまま使える** |
| `array.map { \|item\| expr }` | 各要素のブロック評価結果からなる新しい Array |
| `array.len()` / `len(array)` | 要素数(Int) |
| `len(map)` / `len(str)` | Map のキー数 / Str の文字数 |

- ブロックは必須(`each()` 等はエラー)。ブロック引数の個数は呼び出し側が渡す個数と一致しなければならない(`[1].each { \|a, b\| ... }` はエラー。**空配列でも検証される**)。`each` / `map` / `len` は引数を取らない(`[1].len(99)` 等はエラー)。
- ブロック内で外側に見える名前への代入はエラー(変数の不変性ポリシーはブロックでも同様)。
- `for` 文はない。反復は `each` / `map` を使う。`while` は残すが、変数が再代入不可のため反復には使えない(副作用頼みの待ち合わせ等の特殊用途向け)。

### メソッドと不変性

オブジェクトのメソッドは変更後の**コピーを返す**。レシーバは変化しない。

```ruby
t1 = timestamp(100)
t2 = t1.add_seconds(10)   # t1 は 100 のまま
```

### 制御構文

```ruby
if cond { ... } else if cond { ... } else { ... }   # else / else if 省略可、else if は何段でも可
while cond { ... }
```

ブロックは常に `{ ... }`。`if` / `while` も式として最後に評価した値を返す(`while` が一度も回らなければ nil)。

`if` / `while` の**条件式内では、メソッド呼び出しにブロックを付けられない**(本体 `{ ... }` との区別のため)。`if m.active { ... }` / `if f() { ... }` のような条件は問題なく書けるが、`rows.each { ... }` のようなブロック付き呼び出しは文として書く。

### 組み込み出力

`puts(v, ...)` — 各値を `to_s` 形式で 1 行ずつ印字して nil を返す。`to_s` は Str を素(クォートなし)で出し、それ以外は `inspect` 形式。`inspect` 形式は Ruby 風: `[1, "x"]` / `{"a" => 1}` / `nil` / `"str"`(Str はダブルクォート付き、キー昇順)。`fl` の最終結果は `=> <inspect>` で印字される(トップレベルの Str もクォート付き)。文字列補間 `#{式}` の埋め込みは `to_s` 形式。

## 6. Firestore API

`firestore` オブジェクトは存在しない。すべて**グローバル関数**として提供する。

ドキュメントパスを取る引数(下記 `target`)には、パス文字列(`"users/1"`)または `doc(...)` の戻り値のどちらでも渡せる。

**パスの検証**: ドキュメントパスは**偶数**個、コレクションパスは**奇数**個の非空セグメントで構成されていなければならない(`"users/1"`、`"teams/t1/members"`)。違反(奇偶の不一致、空セグメント、先頭・末尾の `/`)は `doc()` / `col()` / `col_group()` / `get()` / `set()` / `update()` / `delete()` の呼び出し時点でエラーになり、in-memory / emulator で同じエラーになる。

| 関数 | 戻り値 | 説明 |
|------|--------|------|
| `doc(path)` | DocRef | ドキュメント参照を作る |
| `col(path)` | CollectionRef | コレクション参照。`"users"` や `"teams/t1/members"`(サブコレクション)が使える |
| `col_group(id)` | CollectionGroupRef | 同名コレクション横断の参照 |
| `get(target)` | Map / Nil | ドキュメント取得。**存在しなければ nil**(エラーにはならない) |
| `set(target, data_map)` | Nil | ドキュメント全体を書き込み(置換)。キーはリテラルのフィールド名(`.` を含むキーはそのまま保存される) |
| `set(target, data_map, merge_bool)` / `set(target, data_map, merge: true)` | Nil | merge=true ならフィールドマージ。キーは `.` 区切りの**フィールドパス**として解釈(update と同じ) |
| `update(target, data_map)` | Nil | フィールド更新。**ドキュメントが存在しなければエラー**(作成しない)。キーは `.` 区切りの**フィールドパス**として解釈し、ネストしたフィールドを更新する(中間 Map は必要なら作成、中間が Map でなければエラー)。パスの各セグメントは Firestore の単純フィールドパス(`[A-Za-z_][A-Za-z0-9_]*`)のみで、バッククォートによるエスケープは非対応 |
| `delete(target)` | Nil | ドキュメント削除。存在しなくてもエラーにならない |
| `array(...)` | Array | `[...]` リテラルと同等 |
| `array_union(array)` | センチネル | 配列フィールドへの追加(重複は追加されない) |
| `array_remove(array)` | センチネル | 配列フィールドからの除去 |
| `server_timestamp()` | センチネル | 書き込み時にサーバ時刻に解決 |
| `delete_field()` | センチネル | `update` / merge `set` でフィールドを削除する。存在しないフィールドへの指定は no-op |
| `increment(n)` | センチネル | 数値フィールドに `n`(Int)を加算する。フィールドが存在しないか数値以外の場合は `n` を設定。正負のオーバーフロー時は `i64::MAX` / `i64::MIN` に飽和する(Firestore 仕様準拠) |
| `batch { ... }` | ブロックの値 | 8 章参照 |
| `transaction { ... }` | ブロックの値 | 8 章参照 |

`data_map` の値に使えるのは: Int / Str / Bool / Nil / Array / Map / Timestamp、および直下フィールドのセンチネル 5 種。

センチネル 5 種(ServerTimestamp / ArrayUnion / ArrayRemove / DeleteField / Increment)は `set` / `update` の data_map の**直下のフィールド値**としてのみ意味を持つ。ネストした Map / Array の中では使えない(エラー: `field transforms cannot be nested`)。`delete_field()` は `update` と merge `set` でのみ使え、置換の `set` ではエラー(`delete_field() cannot be used with set() without merge`)。

```ruby
set(doc("users/1"), {name: "alice", age: 20})
set("users/1", {age: 21}, true)                       # merge
update("users/1", {tags: array_union(["a"])})
update("users/1", {"profile.name": "b"})              # ドット区切りのフィールドパスでネスト更新
```

update と merge `set` のキーはフィールドパスとして解釈されるため、`{"profile.name": "b"}` は `profile` マップ内の `name` を更新する(in-memory / emulator で同じ挙動)。キーの各セグメントは単純フィールドパス([A-Za-z_][A-Za-z0-9_]*)のみで、それ以外(`` `a.b` `` のようなバッククォート、空セグメント、記号)はエラー。この検証は**センチネル値(`array_union` 等)を置いたキーにも適用される**。置換の `set` では `{"a.b": 1}` のようなキーはそのままリテラルのフィールド名として保存され、センチネル(`array_union` 等)をドット付きキーに置いた場合もリテラルキーに対して解決される(両バックエンドで同じ)。

## 7. クエリ

```ruby
col("users").where("age", ">=", 20).where("active", "==", true).order_by("age", "desc").limit(10).get()
col_group("members").where("active", "==", true).get()
col("users").get()          # 直下の全ドキュメント
col("users").limit(10).offset(5).get()
col("users").where("age", ">=", 20).count()   # 一致件数
col("users").doc("1")       # doc("users/1") と同等
```

- `where(field, op, value)` — op は `"=="` `"!="` `">"` `">="` `"<"` `"<="` `"in"` `"not-in"` `"array_contains"` `"array_contains_any"` の 10 種。複数 `where` は AND。
  - 未対応の op を渡すと `where()` 呼び出し時に即エラー(`unsupported query op: ...`)。
  - `==` は任意の値の構造比較。大小比較(`>` 等)は**同型のフィールド同士のみ**マッチ:Int は数値順、Str は辞書順、Bool は `false < true`、Timestamp は秒。型が異なる・Nil・存在しないフィールドはマッチしない。
  - `!=` はフィールドが存在し、値が構造比較で異なる場合にマッチ(存在しないフィールドはマッチしない。型が異なる値は不一致とみなす)。
  - `in` / `not-in` — `value` は Array(要素は最大 10 個)。`in` はフィールド値がいずれかの要素と等しい場合、`not-in` はフィールドが存在し、どの要素とも等しくない場合にマッチ(ただし Firestore 仕様に準拠し、比較配列に `nil` が含まれる `not-in` は何もマッチしない)。空配列の `in` は何もマッチせず、空配列の `not-in` はフィールドを持つ全ドキュメントにマッチする。Array でない値を渡すとクエリ実行時にエラー。
  - `array_contains` — フィールドが Array で `value` を要素に含む場合にマッチ。`array_contains_any` — `value` は Array(要素は最大 10 個)で、フィールドの Array と 1 つ以上共通の要素を持つ場合にマッチ(空配列は何もマッチしない)。Array でない値を渡すとクエリ実行時にエラー。
  - **文字列フィルタ値のサイズ制限**: Firestore と同様に、`where` の文字列フィルタ値は **1,500 UTF-8 バイト未満**であること(`in` 等の配列要素・`array_contains` の要素を含む。1,500 バイト以上のフィルタ値はクエリ実行時にエラー:`value for {field} is too large to be used in a query`)。**保存値には長さ制限がない**。有効なフィルタ(< 1,500 バイト)に対しては、保存値との最初の差異位置が常にフィルタ自身の長さ以内に収まるため、全文比較で Firestore と同じ結果になる。in-memory / emulator は同じ検証をクエリ実行時に行う。
  - `where` の値には `doc("users/1")` などの `DocRef` も指定可能(同一データベース内の参照型フィールドに対する `==` や `in`、`array_contains` 等のクエリ。emulator バックエンドでは同一プロジェクト・同一データベースの完全リソース名へエンコードされて実行される)。
  - 異なる型同士の比較は未規定(§13)。in-memory ではマッチしない。
  - フィールドが存在しないドキュメントはマッチしない(`!=` / `not-in` を含む)。
- `order_by(field, dir)` — `dir` は `"asc"` / `"desc"`(それ以外は呼び出し時にエラー)。複数回呼ぶと順に適用される。対象フィールドを持たないドキュメントは結果から除外される(`__name__` は全ドキュメントが持つ)。同値は firelang 相対ドキュメントパス文字列の辞書順でタイブレークする。同型の値は where と同じ順序(Int 数値順 / Str 辞書順 / Bool `false < true` / Timestamp 秒)で比べ、型が異なる値同士は Nil < Bool < Int < Timestamp < Str < Array < Map の順に並べる(両バックエンド共通)。順序付けは取得後に両バックエンド共通の比較器で適用するため、サーバ側の複合インデックス制約は受けない。
- `limit(n)` — 件数上限。後勝ち(複数回呼ぶと最後の値)。`n` は 0 以上で、負数はエラー(`limit must be >= 0`)。
- `offset(n)` — 先頭から `n` 件を飛ばす。後勝ち。`n` は 0 以上で、負数はエラー(`offset must be >= 0`)。件数を超える offset は空結果になる。
- `get()` — 実行して `Array<Map>` を返す。各 Map には `__name__` キーで**相対ドキュメントパス**(例 `"users/1"`)が入る。
- `count()` — フィルタに一致する件数を Int で返す。順序・offset・limit は無視する。
- **決定的順序**: `order_by` がなければ結果は firelang 相対ドキュメントパス文字列の辞書順(`users/a` < `users/b`)。`order_by` があれば上記の順序。`offset` → `limit` の順に適用される。
- `col()` のクエリは**直下のドキュメントのみ**対象(サブコレクション内は含まない)。`col_group(id)` はパス中のいずれかの階層でコレクション名が `id` に一致するドキュメントすべてが対象。
- クエリの**構築**(`.where()` / `.order_by()` / `.offset()` / `.limit()`)は `batch` / `transaction` 内でも可。**実行**(`.get()` / `.count()`)のみ不可。
- カーソル(`start_after` 等)は非対応(12 章)。ページネーションは `order_by` + `offset` + `limit` で行う。

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
| `ts.to_rfc3339()` | RFC3339 文字列(UTC、`+00:00`)を返す |
| `ts.add_seconds(n)` | n 秒後の**新しい** Timestamp を返す |
| `t1 > t2` / `<` / `>=` / `<=` / `==` / `!=` | Timestamp 同士の比較(秒で比較) |
| `t2 - t1` | 秒差を Int で返す。範囲外は `integer overflow` エラー |

## 10. CLI (`fl`)

```
usage: fl [--backend in-memory|emulator] <script-file>
```

- スクリプトファイルはちょうど 1 つ。`--help` / `-h` で usage 表示。未知のオプション・未知のバックエンドはエラー終了。
- `--backend emulator` のとき:
  - `FIRESTORE_EMULATOR_HOST` — **必須**。未設定なら即エラー。
  - `FIRESTORE_PROJECT_ID` — 省略時 `firelang-test`。
- 実行結果は `=> <inspect表現>` で印字。
- エラーは `error: <line>:<col>: <message>` を stderr に印字して exit 1。lex / parse エラーは該当トークンの位置、runtime / backend エラーは失敗したトップレベル文の開始位置。位置が付かない場合(補間内の単一式制約違反など)は `error: <message>`。

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
- ただし emulator 側に firelang 外の型(`double` / `reference` / `geo_point` / `bytes`)で書かれたフィールドがある場合、emulator バックエンドは `Opaque` / `DocRef` に読み替えて返す(§3)。in-memory にはそのような値は存在しない。

## 12. スコープ外(MVP)

以下は明示的に**非対応**。実装しない・README で匂わせない。

- Float(数値は Int のみ)
- 本番 Firestore への認証付きアクセス
- transaction の自動リトライ
- カーソルによるページネーション(`start_after` 等。`order_by` + `offset` + `limit` で代替)
- リスナー(リアルタイム購読)・インデックス依存機能
- ドキュメントに書かれたサブセットを超える mruby 互換

## 13. 既知の制限・実装メモ

実装済みコードから確認された、仕様としては保証しない・将来直す予定の挙動。

| 項目 | 現状の挙動 | 予定 |
|------|-----------|------|
| `where` の異なる型同士の大小比較 | in-memory ではマッチしない。emulator では Firestore の型順序で比較される(§7 は同型のみを規定) | backlog |
