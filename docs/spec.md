# Flare Language Specification / API Reference

This document is the **sole authoritative specification** for flare. If the README, examples, or tests disagree with it, update the implementation or documentation to match this specification. Update this document in the same commit whenever behavior changes.

Target version: 0.1.0 (MVP)

- [1. Overview](#1-overview)
- [2. Script Structure](#2-script-structure)
- [3. Values and Types](#3-values-and-types)
- [4. Literals and Expressions](#4-literals-and-expressions)
- [5. Variables, Functions, and Control Flow](#5-variables-functions-and-control-flow)
- [6. Firestore API](#6-firestore-api)
- [7. Queries](#7-queries)
- [8. Batch / Transaction](#8-batch--transaction)
- [9. Timestamp API](#9-timestamp-api)
- [10. CLI (`fl`)](#10-cli-fl)
- [11. Auth API](#11-auth-api)
- [12. Backends](#12-backends)
- [13. Execution Budgets and Trust Boundaries](#13-execution-budgets-and-trust-boundaries)
- [14. Out of Scope (MVP)](#14-out-of-scope-mvp)
- [15. Known Limitations and Implementation Notes](#15-known-limitations-and-implementation-notes)
- [16. Cloud Storage API](#16-cloud-storage-api)

## 1. Overview

flare is an mruby-inspired scripting language for working with Firestore. It is not fully compatible with mruby; it provides only the lightweight subset needed for Firestore operations.

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"
```

## 2. Script Structure

- Source code is UTF-8 text. Statements are separated by a newline or `;`.
- Newlines are ignored **inside** map literals `{ ... }`, array literals `[ ... ]`, argument lists `( ... )`, function and block parameter lists, grouping parentheses `( expr )`, and index brackets `[ ... ]`, as in Ruby. Newlines within an **expression** used as an element, value, or argument are also ignored, so literals and argument lists may span multiple lines. Newlines before or after a **binary operator or postfix** `.` / `[]` remain part of the same expression. **Trailing commas** are allowed (`{a: 1,}`, `[1, 2,]`, `f(a, b, )`):
  ```ruby
  set("users/1", {
    name: "a",
    tags: [
      "x",
      "y",
    ],
  })
  [1
    + 2]
  m = {value: 1}
  [
    m
      .value
  ]
  ```
  In contrast, newlines remain statement separators inside `{ ... }` statement blocks for `fn`, `if`, `while`, and methods with blocks. Newlines outside delimiters also separate statements. Inside delimiters, `;` is not equivalent to a newline; elements are separated only by commas, so forms such as `[;1]` are errors.
- `#` starts a comment that continues to the end of the line, except inside string literals.
- The program result is the value of the last evaluated statement. If `return` executes at the top level, its value is the result. A `return` propagated out of a block also stops the remaining top-level statements and becomes the program result.
- The `fl` CLI prints the evaluation result to stdout as `=> <inspect representation>`.

## 3. Values and Types

| Type | Example | Description |
|----|----|------|
| Int | `42` | 64-bit signed integer. **Numbers are integers only** (Float is unsupported). |
| Str | `"hello"` | UTF-8 string |
| Bytes | `bytes([0, 255])` | Arbitrary byte sequence independent of UTF-8 strings; Storage content round-trips losslessly. |
| Bool | `true` / `false` | Boolean |
| Nil | `nil` | No value. `get` for a missing document also returns nil. |
| Array | `[1, 2, "a"]` | Sequence of arbitrary values |
| Map | `{name: "a", age: 1}` | Dictionary with string keys, stored in key order |
| Timestamp | `timestamp(100)` / `now()` | Unix seconds (integer); no nanosecond precision |
| Bytes | `bytes("fire")` / `bytes([102, 105, 114, 101])` | Immutable byte sequence. Strings become UTF-8 bytes; every integer array element must be in `0..=255`. |
| Func | `fn f(a) { a }` | User-defined function |
| BuiltinModule | `Firestore`, `Auth`, `Storage` | Immutable namespace value whose methods use the current Runtime's configured service |

Special values for Firestore operations:

| Type | Creation | Description |
|----|---------|------|
| DocRef | `doc("users/1")` | Document reference |
| CollectionRef | `col("users")` | Collection reference |
| CollectionGroupRef | `col_group("members")` | Collection group reference |
| QueryRef | `col(..).where(..)`, etc. | Query under construction |
| ServerTimestamp | `server_timestamp()` | Sentinel resolved to the commit request time. All such sentinels in one write / batch / transaction commit use the same time. |
| ArrayUnion / ArrayRemove | `array_union([..])` / `array_remove([..])` | Array field transform sentinels |
| DeleteField | `delete_field()` | Field deletion sentinel (`update` and merge `set` only) |
| Increment | `increment(n)` | Numeric increment sentinel |
| Opaque | (Cannot be created; emulator reads only) | Display-only value for field types not supported by flare, such as `double` / `geo_point` (`Double(1.5)`, etc.). Cannot be written. |
| AuthSession | `auth_sign_up(...)` / `auth_sign_in(...)` | Auth session containing a user record and its ID / refresh token. Tokens are redacted by `inspect` (§11). |
| RedactedToken | (Cannot be created; access `session.id_token`) | Redacted token projection (`<redacted:id_token>`). The raw token is never exposed to scripts (§11). |

The five sentinels (ServerTimestamp / ArrayUnion / ArrayRemove / DeleteField / Increment) are valid only as **direct field values** in the `data_map` of `set` / `update`. They are invalid inside nested Maps / Arrays (both backends return an error). Before staging a write, `set` / `update` recursively validate the data map and reject values Firestore cannot store, such as `Func`, queries, or `Opaque`, without sending them to the backend (`unsupported value in write`).

The five sentinels (ServerTimestamp / ArrayUnion / ArrayRemove / DeleteField / Increment) are valid only as **direct field values** in the `data_map` of `set` / `update`. They are invalid inside nested Maps / Arrays (both backends return an error).

When the emulator backend reads a document containing field types flare does not support, it converts them instead of rejecting the entire document: `bytes` becomes a lossless first-class `Bytes` value; `double` / `geo_point` become display-only `Opaque` values (for example, `Double(1.5)` or `GeoPoint(35.7, 139.7)`); and a `reference` in the same database becomes a relative-path `DocRef` (references to another project or database become an `Opaque` `Reference(...)` value). `Opaque` values cannot be matched by query operators such as `==` (scripts cannot create them), and writing them back fails with `unsupported value in emulator write`. Storage `bytes([...])` is `Value::Bytes`; downloaded bytes retain their individual values.

### Truthiness

Only `false` and `nil` are false. Everything else is true, including `0`, `""`, and `[]`.

## 4. Literals and Expressions

### Integer Literals

Only decimal notation is supported. Write negative values with unary minus (`-5`, `-(a + b)`). The lexer reports `integer literal out of range` for literals outside the i64 range. `-9223372036854775808` (`i64::MIN`) cannot be written as a literal because its positive part is out of range; write it as `0 - 9223372036854775807 - 1`. If unary negation overflows (for example, applying `-` to `i64::MIN`), it reports `integer overflow`.

### String Literals

Only double-quoted strings are supported. Escapes: `\n`, `\r`, `\t`, `\"`, and `\\`. For any other `\x`, the `x` remains unchanged.

String interpolation with `#{expression}` is supported:

```ruby
name = "fire"
"hello #{name} #{1 + 2}"  # => "hello fire 3"
```

- The contents of `#{}` must be **one expression**; statements or multiple expressions are errors.
- Write `\#{` to escape `#{` and include it literally.
- Interpolation inside a map literal **key** (`{"k#{1}": v}`) is an error.

### Array and Map Literals

```ruby
[1, 2, "a"]
{name: "alice", "with space": 1, nested: {a: [1]}}
```

Map keys must be identifiers or string literals. Values may be any expression.

### Byte Sequences

`bytes([0, 255, 128])` creates `Bytes` from an array of Int values in `0..=255`. There is no implicit conversion from string literals to UTF-8 byte sequences.

### Indexing (`expr[expr]` and `m.key`)

Maps and Arrays support indexing. Indexing is a postfix operation like a method call, can be chained, and works on any expression, including the result of `get()`:

```ruby
m = {name: "alice", nested: {a: [1, 2]}}
m["name"]          # => "alice"
m["nested"]["a"][1]  # => 2
m.name             # => "alice" (syntactic sugar for m["name"] on Maps only)

rows = col("users").get()
rows[0]["__name__"]  # => "users/1" (query-result Map indexing; __name__ is accessed the same way)
```

- Map indices must be **Str**; Array indices must be **Int**. A type mismatch is an error.
- Missing keys and out-of-range indices return **nil**, as in Ruby. Negative indices also return nil.
- Indexing a receiver other than Map or Array is an error (`unsupported index receiver`).
- The `m.key` form is limited to Map receivers to avoid conflicts with methods such as `.doc(..)` / `.where(..)` on Ref / Query values. A missing key returns nil. Calling it with arguments (`m.key(...)`) is an error.

### Operators and Precedence (Low to High)

| Precedence | Operator | Supported types |
|----------|--------|--------|
| 1 (lowest) | `\|\|` | Any value (short-circuits and returns the operand itself, as in Ruby) |
| 2 | `&&` | Any value (short-circuits and returns the operand itself) |
| 3 | `==` `!=` | Any values (structural equality) |
| 3 | `>` `<` `>=` `<=` | Int / Timestamp / Bytes pairs (Bytes use unsigned-byte lexicographic order); other types are errors |
| 4 | `+` `-` | `+`: Int pairs / Str pairs (concatenation). `-`: Int pairs / Timestamp − Timestamp → Int (difference in seconds). Other types are errors |
| 5 | `*` `/` | Int only. `/ 0` reports `division by zero`. |
| 6 | `!` `-` (unary) | `!v` accepts any value and returns `Bool(!truthy(v))`. `-v` accepts Int only (`integer overflow` on overflow). |
| 7 (highest) | `.method(...)` / `expr[expr]` | Method calls and indexing (see §4, Indexing) |

- `&&` returns its left operand if false (`false` / `nil`) without evaluating the right operand; otherwise it evaluates and returns the right operand.
- `||` returns its left operand if truthy without evaluating the right operand; otherwise it evaluates and returns the right operand.
- Unary `!` / `-` are right-associative: `!!x` means `!(!x)` and `--5` means `-(-5)`. The lexer distinguishes `!=` (`a != b` is a comparison) from `!` (`!a` is negation).
- Truthiness follows §3: only `false` and `nil` are false.
- Unary minus cannot be used as the first argument of a call without parentheses. `puts -1` is parsed as `puts - 1` (subtraction), so use parentheses to pass a negative value (`puts(-1)`). `!` is allowed (`puts !x` means `puts(!x)`).

If an arithmetic operation (`+`, `-`, `*`, `/`) on Int values exceeds the i64 range, it reports `integer overflow` (it does not panic in debug builds). `/ 0` reports `division by zero`.

### Expression Evaluation Order for Calls

- Reject duplicate keywords before evaluating the receiver. Then resolve the receiver exactly once, validate the call contract and service/context requirements, and only then evaluate arguments. A function call without a receiver also validates duplicate keywords and its call contract before evaluating arguments.
- Evaluate arguments, positional and keyword alike, **from left to right in source order**. The parser preserves them as one ordered sequence and allows them to be interleaved. Evaluate each argument expression exactly once.
- Evaluation order and parameter binding order are separate. In a function call, positional values fill parameters from left to right first; keywords then fill matching parameters by name. Thus, for `fn f(b, a) { ... }`, `f(a: first(), second())` evaluates `first()` then `second()`, but binds the positional value to `b` and the keyword value to `a`.
- If evaluating a receiver or argument expression fails, return that error without evaluating the remaining expressions. If receiver evaluation or receiver type validation fails, later argument side effects do not occur. If the receiver succeeds and a later argument fails, side effects already caused by evaluating the receiver remain.

## 5. Variables, Functions, and Control Flow

### Variables (Immutable)

```ruby
a = 1
a = 2   # Error: reassignment is not allowed: a
```

- Variables cannot be reassigned. Function names share the same namespace and cannot be redefined.
- Defining a function with a built-in name (`puts`, `get`, `set`, `update`, `delete`, `batch`, `transaction`, `doc`, `col`, `col_group`, `array`, `len`, `now`, `timestamp`, `bytes`, `server_timestamp`, `array_union`, `array_remove`, `delete_field`, `increment`, `storage_upload`, `storage_download`, `storage_metadata`, `storage_delete`, `auth_sign_up`, `auth_sign_in`, `auth_current_user`, `auth_sign_out`) errors with `cannot redefine builtin: <name>`. Built-in calls always take precedence.
- Assigning to a name visible from an outer scope inside a function errors; shadowing is not allowed. **Function parameters** may have the same names as outer variables.
- Parameter names cannot repeat within a function or block definition. Duplicate parameters cause a parse or definition error.

### Scope Rules (Static Scope)

A function can see only the **global scope and its parameters**. It cannot see the caller’s local variables or locals defined in another function (static scope, as in Ruby):

```ruby
fn f() { x }
fn g() { x = 1
  f() }
g()      # Error: undefined variable: x
```

- Functions can read global variables and recurse by referring to a globally defined function.
- Assigning to an already visible name (global, parameter, or function local) inside a function errors. A name that exists only in the caller can be defined as a function local.
- Block parameters (`each`, `map`, etc.) follow the same rule and may reuse outer names. Block bodies can read outer-scope variables because they are evaluated in the caller’s environment.

### Function Definitions and Calls

```ruby
fn add(a, b) { return a + b }   # return is optional (the last expression is the result)
add(2, 3)
add(a: 2, b: 3)                 # Ruby-style keyword arguments
puts "x"                        # Parentheses may be omitted
```

- Positional and keyword arguments can be mixed. Positional values bind first, then keywords bind by name. Missing, extra, or unknown arguments error.
- Built-in module calls accept the same keyword arguments as their global aliases. Other receiver methods keep their documented contracts; query and collection methods do not accept keywords.
- The parameter list may be omitted, as in `fn name { ... }`.
- An array literal cannot be the **first argument of a call without parentheses**. `puts [1, 2]` is parsed as indexing `puts[1, 2]`; pass arrays with parentheses, as `puts([1, 2])`.

### Method Calls with Blocks and Iteration

Method calls can take a block with `|params|`. Block parameters are bound in a new scope and may reuse outer variable names, following the parameter rules above. Since variables cannot be reassigned, use `map` to collect iteration results:

```ruby
rows = col("users").where("active", "==", true).get()
rows.each { |row| update(row["__name__"], {seen: true}) }   # Side effect for each item; returns nil
doubled = [1, 2, 3].map { |n| n * 2 }                       # New Array with each item transformed
total = len(rows)                                            # Number of items
```

| Expression | Description |
|----|------|
| `array.each { \|item\| ... }` | Evaluates the block with each item bound to its parameter; returns nil. **Can be used directly on query results (`.get()`).** |
| `array.map { \|item\| expr }` | Returns a new Array of block results for each item |
| `array.len()` / `len(array)` | Number of items (Int) |
| `len(map)` / `len(str)` / `len(bytes)` | Number of Map keys / Str characters / Bytes bytes |

- A block is required (`each()` without one errors). The number of block parameters must match the number supplied by the caller (`[1].each { \|a, b\| ... }` errors; **this is checked even for empty arrays**). `each`, `map`, and `len` take no arguments (`[1].len(99)` errors).
- Assigning to an outer-scope name inside a block errors, consistent with variable immutability.
- A `return` inside a block exits the block and becomes the return value of the **enclosing function**. Iteration in `each` / `map` stops there, so later items are not processed. This propagation applies only when returning from a block to an outer function. A `Return` within the function itself (the body or a block in that body) is consumed as that function’s result. A nested block’s `return` belongs to the innermost **function**.
- There is no `for` statement; use `each` / `map` to iterate. `while` remains, but immutable variables make it unsuitable for iteration (it is intended for special cases such as waiting on side effects).

### Methods and Immutability

Object methods return a **modified copy**. The receiver is unchanged.

```ruby
t1 = timestamp(100)
t2 = t1.add_seconds(10)   # t1 remains 100
```

### Control Flow

```ruby
if cond { ... } else if cond { ... } else { ... }   # else / else if are optional; else if may be repeated
while cond { ... }
```

Blocks always use `{ ... }`. `if` and `while` are expressions that return the last evaluated value (`while` returns nil if it never runs).

A method call inside an `if` / `while` **condition cannot have a block**, to distinguish it from the body `{ ... }`. Conditions such as `if m.active { ... }` and `if f() { ... }` are valid, but calls with blocks such as `rows.each { ... }` must be written as statements.

### Built-in Output

`puts(v, ...)` prints each value on its own line using `to_s` and returns nil. `to_s` prints Str without quotes and all other values in `inspect` form. `inspect` is Ruby-like: `[1, "x"]`, `{"a" => 1}`, `nil`, and `"str"` (Str is double-quoted; Map keys are sorted). Bytes are shown as hex, up to the first 32 bytes, as `Bytes(00ff)`. Longer values use `Bytes(<first 32 bytes>, <omitted byte count> more bytes)`; Bytes are not decoded as Unicode. The final `fl` result prints as `=> <inspect>` (top-level Str values are quoted too). Interpolation (`#{expression}`) embeds `to_s` output, so Bytes use the same bounded `Bytes(...)` representation.

## 6. Firestore API

There is no `firestore` object. All operations are provided as **global functions**.

Arguments that accept a document path (the `target` argument below) accept either a path string (`"users/1"`) or the result of `doc(...)`.

**Path validation:** Document paths must contain an **even** number of non-empty segments; collection paths must contain an **odd** number (for example, `"users/1"` and `"teams/t1/members"`). Every segment must be a valid Firestore document / collection ID: at most **1,500 UTF-8 bytes**, no `/`, not exactly `.` or `..`, and not matching the reserved `__.*__` pattern. The Datastore numeric-ID form `__id[0-9]+__` is allowed as an existing reference. The `id` in `col_group(id)` and `col(...).doc(id)` has the same constraints. Violations (wrong parity, empty segments, leading/trailing `/`, or invalid IDs) error when `doc()`, `col()`, `col_group()`, `get()`, `set()`, `update()`, or `delete()` is called; in-memory and emulator backends return the same error.

| Function | Return value | Description |
|------|--------|------|
| `doc(path)` | DocRef | Creates a document reference |
| `col(path)` | CollectionRef | Creates a collection reference; accepts paths such as `"users"` or `"teams/t1/members"` (a subcollection) |
| `col_group(id)` | CollectionGroupRef | Reference spanning collections with the same name |
| `get(target)` | Map / Nil | Gets a document. Returns **nil if it does not exist** (not an error). |
| `set(target, data_map)` | Nil | Writes (replaces) the whole document. Keys are literal field names, so a key containing `.` is stored unchanged. |
| `set(target, data_map, merge_bool)` / `set(target, data_map, merge: true)` | Nil | With `merge=true`, merges fields. Keys are interpreted as **field paths** separated by `.`, as with `update`. |
| `update(target, data_map)` | Nil | Updates fields. Errors if the **document does not exist** (does not create it). Keys are interpreted as **field paths** separated by `.`, and update nested fields (intermediate Maps are created as needed; an intermediate value that is not a Map is an error). Each path segment must be a simple Firestore field path (`[A-Za-z_][A-Za-z0-9_]*`); backtick escaping is unsupported. |
| `delete(target)` | Nil | Deletes a document; does not error if it is missing. |
| `array(...)` | Array | Equivalent to an `[...]` literal |
| `bytes(str)` / `bytes(int_array)` | Bytes | Creates an immutable byte sequence from a string's UTF-8 bytes or an array of Int values in `0..=255`. Exactly one argument is required. |
| `array_union(array)` | Sentinel | Adds values to an array field (duplicates are not added) |
| `array_remove(array)` | Sentinel | Removes values from an array field |
| `server_timestamp()` | Sentinel | Resolves to server time when written. All sentinels in one ordinary write or one batch / transaction commit use the same request time. |
| `delete_field()` | Sentinel | Deletes a field in `update` / merge `set`. Specifying a missing field is a no-op. |
| `increment(n)` | Sentinel | Adds `n` (Int) to a numeric field. If the field is missing or not numeric, sets it to `n`. Positive and negative overflow saturate at `i64::MAX` / `i64::MIN`, matching Firestore. |
| `batch { ... }` | Block value | See §8 |
| `transaction { ... }` | Block value | See §8 |

Values allowed in `data_map`: Int / Str / Bool / Nil / Array / Map / Timestamp / Bytes, plus the five sentinels as direct field values.

Values written to Firestore are validated both before the runtime stages a write and at the write boundary by a recursive validator shared by the in-memory / emulator backends. If any write in a batch / transaction is rejected during staging, nothing is committed. Validation rejects values Firestore cannot store (`Func`, queries, `Opaque`, and `DocRef` on the emulator). Firestore constraints:

- An `Array` cannot directly contain another `Array` (`[[1]]` reports `Nested arrays are not allowed`). Indirect nesting through a Map is allowed (`Array -> Map -> Array`), for example `{rows: [{values: [1, 2]}]}.
- Map / document field names cannot be empty or match Firestore's reserved pattern `__.*__`. Check empty and reserved names at every Map nesting level (errors: `The property.name is the empty string.` / `field name <name> is reserved`). Field names are limited to 1,500 UTF-8 bytes.
- In field paths for `update` / merge `set`, apply the same name restrictions to each simple field-name segment (`__name__` and other reserved names are disallowed).

The five sentinels (ServerTimestamp / ArrayUnion / ArrayRemove / DeleteField / Increment) are valid only as **direct field values** in the `data_map` of `set` / `update`. They are invalid inside nested Maps / Arrays (error: `field transforms cannot be nested`). `delete_field()` is valid only with `update` and merge `set`; replacement `set` reports `delete_field() cannot be used with set() without merge`.

`server_timestamp()` uses the commit request time. Each ordinary `set` / `update` is one write request; `batch` / `transaction` each use one commit request. **All `ServerTimestamp` sentinels in one request resolve to the same Timestamp.** Separate ordinary writes are separate requests and need not have the same Timestamp. This matches the Firestore emulator `REQUEST_TIME` transform and differs from `now()`, which returns the current time on each call.

```ruby
set(doc("users/1"), {name: "alice", age: 20})
set("users/1", {age: 21}, true)                       # merge
update("users/1", {tags: array_union(["a"])})
update("users/1", {"profile.name": "b"})              # Nested update using a dot-separated field path
```

Keys in `update` and merge `set` are field paths, so `{"profile.name": "b"}` updates `name` inside the `profile` Map; in-memory and emulator behave the same. Each segment must be a simple field path (`[A-Za-z_][A-Za-z0-9_]*`); backtick-escaped `` `a.b` ``, empty segments, and punctuation are errors. **This validation also applies to keys containing sentinels such as `array_union`.** Duplicate or overlapping paths within deletion/ordinary-value masks are rejected before sending to the backend. Overlap with transforms and ancestor/descendant operation order are allowed as specified. In replacement `set`, keys such as `{"a.b": 1}` are stored literally. A sentinel on a non-simple key also resolves against that literal key; both backends behave the same. The emulator uses Firestore's quoted field-path representation and escapes backslashes and backticks accordingly.

## 7. Queries

```ruby
col("users").where("age", ">=", 20).where("active", "==", true).order_by("age", "desc").limit(10).get()
col_group("members").where("active", "==", true).get()
col("users").get()          # All direct child documents
col("users").limit(10).offset(5).get()
col("users").where("age", ">=", 20).count()   # Number of matches
col("users").doc("1")       # Equivalent to doc("users/1")
```

- `where(field, op, value)` — `op` must be one of `==`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `not-in`, `array_contains`, or `array_contains_any`. Multiple `where` clauses are combined with AND.
  - An unsupported operator immediately errors when `where()` is called (`unsupported query op: ...`).
  - `==` performs structural comparison on any values (Bytes compare all bytes). `>`, `>=`, `<`, and `<=` compare only pairs of the **same type among Int / Str / Bool / Timestamp / Bytes**: Int numerically, Str lexicographically, Bool with `false < true`, Timestamp by seconds, and Bytes by unsigned-byte lexicographic order. Mixed types, Nil, Array, and Map do not match ordered comparisons (Array / Map have no ordering for `where`, even against the same type). Missing fields do not match.
  - `!=` matches when the field exists and its value is structurally different. A field whose value is `nil` does not match `!= nil`, but does match `== nil`. A missing field does not match. Values of different types are considered unequal. The emulator backend evaluates `!=` client-side instead of filtering through the server RPC, to keep behavior consistent for Double vs. Int comparisons and Opaque values.
  - `in` / `not-in` — `value` must be an Array with at most 10 elements. `in` matches if the field equals any element. `not-in` matches if the field exists and equals none of the elements (per Firestore, `not-in` matches nothing if the comparison array contains `nil`). An empty `in` array matches nothing; an empty `not-in` array matches every document that has the field. A non-Array value errors when the query runs.
  - `array_contains` matches when the field is an Array containing `value`. `array_contains_any` requires an Array `value` with at most 10 elements and matches when the field Array shares at least one element with it (an empty array matches nothing). A non-Array value errors when the query runs.
  - **String filter size limit:** As in Firestore, string values used by `where` must be **under 1,500 UTF-8 bytes**, including array elements for `in` etc., `array_contains` elements, and values nested in Maps / Arrays. A filter value of 1,500 bytes or more errors when the query runs: `value for {field} is too large to be used in a query`. **Stored values have no length limit.** For valid filters (< 1,500 bytes), the first difference from a stored value is always within the filter length, so full comparison produces the same result as Firestore. In-memory and emulator perform the same validation when the query runs.
  - A `DocRef` such as `doc("users/1")` may be used as a `where` value for queries on reference fields in the same database, including `==`, `in`, and `array_contains`. The emulator backend encodes it as a full resource name in the same project and database; nested Array / Map values are encoded recursively by the same rule. A `DocRef` is allowed as a query filter value, but the emulator write codec rejects it with `unsupported value in emulator write`. The shared matcher makes the final decision on server-filtered candidates after normalizing references to relative paths.
  - The emulator may use Firestore server filters only to narrow the candidate set. The final `where` decision is always made client-side by the same implementation used in-memory. Firestore type ordering for comparisons between different types therefore does not affect flare results; in-memory and emulator return the same results.
  - Documents without the field do not match, including for `!=` / `not-in`.
- `order_by(field, dir)` — `dir` must be `"asc"` or `"desc"`; other values error when called. Multiple clauses are applied in order. Documents missing the field are excluded (`__name__` exists on every document). Ties are broken by lexicographic order of the flare-relative document path. Values of the same type use the `where` ordering (Int numeric, Str lexicographic, Bool `false < true`, Timestamp by seconds, Bytes by unsigned-byte lexicographic order). Different types sort as Nil < Bool < Int < Timestamp < Str < Bytes < Array < Map < other, identically for both backends. Ordering uses the shared comparator after retrieval, so server composite-index constraints do not apply.

- `limit(n)` — Maximum result count. The last value wins if specified more than once. `n` must be non-negative; a negative value errors (`limit must be >= 0`).
- `offset(n)` — Skips the first `n` results; the last value wins if specified more than once. `n` must be non-negative; a negative value errors (`offset must be >= 0`). An offset larger than the result count returns an empty result.
- `get()` — Executes the query and returns `Array<Map>`. Each Map contains the **relative document path** (for example, `"users/1"`) under the `__name__` key.
- `count()` — Returns the number of matching documents as Int. Ignores ordering, offset, and limit. In-memory counts references to stored documents directly; the emulator uses a `COUNT(*)` aggregation query.
- **Deterministic order:** Without `order_by`, results are sorted lexicographically by flare-relative document path (`users/a` < `users/b`). With `order_by`, the ordering above applies. Apply `offset` before `limit`.
- **Query execution path:** In-memory `get()` orders references to candidates, then clones only fields included in the page. The emulator sends `__name__` ordering and page bounds to the server only when there is no `order_by`, no range-comparison filter, and offset/limit fit in protobuf i32. Other `get()` calls, and `count()` on emulators without aggregation RPC support, use a safe fallback that returns all candidates and retrieves document payloads outside the page.
- Queries on `col()` include **direct child documents only**, not documents in subcollections. `col_group(id)` includes every document whose collection name matches `id` at any level of its path.
- Query **construction** (`.where()` / `.order_by()` / `.offset()` / `.limit()`) is allowed inside `batch` / `transaction`. Query **execution** (`.get()` / `.count()`) is not.
- Cursors such as `start_after` are unsupported (§12). Use `order_by` + `offset` + `limit` for pagination.

## 8. Batch / Transaction

Both forms require a `{ ... }` block and return its final value. The block runs in a **lexical child scope**: it can read variables from the calling (outer) scope, but assigning to a visible outer name errors because variables are immutable. Variables and functions defined in the block are not visible outside it. The child scope is discarded on success and on runtime or backend errors, so the same Runtime can execute another `batch` or `transaction` after a failure. A `return` inside the block commits or rolls back writes before propagating to the enclosing function. It does not produce a value for a binding such as `x = batch { ...; return }`, because the enclosing function exits immediately. Nested batch/transaction blocks are not allowed (errors: `batch cannot be nested inside batch/transaction` / `transaction cannot be nested inside batch/transaction`).

### `batch`

- **Writes only:** stages `set` / `update` / `delete`. `get` and query execution error.
- On success, applies all writes **atomically**.
- On runtime or backend failure, **no writes are applied**; partial writes are not possible.

### `transaction`

- Supports `get` / `set` / `update` / `delete`.
- `get` may be called any number of times **before the first write is staged**, including repeated reads of one document or reads of multiple documents. Reads use the snapshot from the start of the transaction (the emulator uses reads with a transaction ID).
- **Calling `get` after the first write errors** (`get is not allowed after the first write in a transaction`).
- Query execution is not allowed.
- **Single attempt:** no automatic retries.
- On runtime or backend failure, no writes are applied.

```ruby
transaction {
  v = get("users/1")
  set("users/1", {visits: 1})
}
```

## 9. Timestamp API

| Expression | Description |
|----|------|
| `now()` | Current Timestamp (Unix seconds) |
| `timestamp(1700000000)` | Creates a Timestamp from Unix seconds |
| `timestamp("2026-02-01T00:00:00+09:00")` | Creates a Timestamp from an RFC3339 string. Offsets may be `Z` or `±HH:MM`; fractional seconds are unsupported. |
| `ts.to_unix()` | Returns an Int (Unix seconds) |
| `ts.to_rfc3339()` | Returns an RFC3339 string in UTC (`+00:00`) |
| `ts.add_seconds(n)` | Returns a **new** Timestamp `n` seconds later. Out-of-range values error with `integer overflow`. |
| `t1 > t2` / `<` / `>=` / `<=` / `==` / `!=` | Compares Timestamps by seconds |
| `t2 - t1` | Returns the difference in seconds as an Int. Out-of-range values error with `integer overflow`. |

## 10. CLI (`fl`)

```
usage: fl [--backend in-memory|emulator] [--storage-backend auto|in-memory|emulator] [--auth-backend in-memory|emulator] [--emulator-timeout-ms <ms>] (<script-file> | - | -e <script>)
```

- Specify exactly one script source: a file path, `-` (stdin), or `-e <script>` (inline script; repeated values are joined with newlines). With no arguments, read from piped stdin; with a terminal stdin, print usage and exit. Combining `-e` with a file or `-` is an error. `--help` / `-h` prints usage. Unknown options and unknown backends for either `--backend` or `--auth-backend` exit with an error.
  ```sh
  fl script.fl
  cargo run --features emulator --bin fl -- --backend emulator script.fl
  echo 'get("users/1")' | cargo run --features emulator --bin fl -- --backend emulator -
  fl -e 'col("users").get().len()'
  ```
- Binaries using `--backend emulator` must be built with the `emulator` feature.
  If specified without the feature, the following message is printed and the program exits:
  `emulator backend requires the \`emulator\` Cargo feature; rebuild with --features emulator`
  is printed, then the program exits.
- With `--backend emulator` (when the `emulator` feature is enabled):
  - `FIRESTORE_EMULATOR_HOST` is **required**; missing values error immediately.
- `--storage-backend` selects Storage: `auto` (default), `in-memory`, or `emulator`. `auto` uses the Storage emulator when `FIREBASE_STORAGE_EMULATOR_HOST` is set, otherwise in-memory. Explicit `emulator` requires `FIREBASE_STORAGE_EMULATOR_HOST`.
- With `--auth-backend emulator`:
  - `FIREBASE_AUTH_EMULATOR_HOST` is **required**; missing values error immediately.
- The CLI resolves `FIREBASE_PROJECT_ID`, `GCLOUD_PROJECT`, and `FIRESTORE_PROJECT_ID` once. Repeated identical values are accepted; conflicting values are a configuration error. If none is set, the project is `flare-test`. This one project is passed to each selected emulator backend. `FIREBASE_STORAGE_BUCKET`, when set, overrides the Storage emulator's default `{project_id}.appspot.com` bucket.
- Auth emulator requests use a 5,000 ms operation timeout and `fake-api-key` by default. `FIREBASE_API_KEY` overrides the key. Rust callers can set `AuthEmulatorOptions` explicitly; that constructor does not read environment variables.
- Auth and Firestore emulator clients are synchronous adapters that own a Tokio runtime. Construct and call them from synchronous code, not inside an active Tokio runtime; async hosts should move construction and script execution into a blocking task when the backend types permit it. Storage uses synchronous socket I/O and synchronous DNS lookup.
- Firestore and Auth backends can be selected independently. If both use `emulator`, both emulators are connected to.
- Backend configuration is validated before reading stdin or a file, so an invalid configuration will not leave the CLI waiting for stdin.
- The result is printed as `=> <inspect representation>`.
- Errors print `error: <line>:<col>: <message>` to stderr and exit with status 1. Lex / parse errors use the offending token position; runtime / backend errors use the start of the failed top-level statement. Errors without a position (such as violations of the single-expression rule inside interpolation) print `error: <message>`.

## 11. Auth API

Authentication is provided through global functions, like Firestore. The Auth backend is independent of the Firestore backend and defaults to in-memory; select the emulator with `--auth-backend emulator` or `AuthEmulator`.

The `Auth` module is an alias for the same handlers: `Auth.sign_up(email, password)`, `Auth.sign_in(email, password)`, `Auth.current_user()`, and `Auth.sign_out()`. The `Firestore` module exposes the Firestore global functions as methods, and `Storage` exposes the Storage global functions. Module values can be assigned or passed to functions; they contain no backend or credentials. Local variables and functions take precedence over module names.

Rust embedders can compose services with `Services::in_memory()`, `with_firestore`, `with_auth`, `with_storage`, and `with_project`, then create a runtime with `Runtime::from_services`. `Services::empty(project)` leaves every service unavailable. A call requiring an unavailable service returns a typed `CapabilityError` before evaluating its arguments. Reference builders and Firestore sentinels remain usable without a Firestore backend. A configured Auth backend with no signed-in user returns `nil` from `Auth.current_user()`.

| Function | Return value | Description |
|------|--------|------|
| `auth_sign_up(email, password)` | AuthSession | Creates and signs in a new email/password user. Errors if the user already exists. |
| `auth_sign_in(email, password)` | AuthSession | Signs in an existing user. |
| `auth_current_user()` | Map / Nil | Returns the current session user record, or `nil` if signed out. |
| `auth_sign_out()` | Nil | Discards the local session. Idempotent; does not error when already signed out. |

```ruby
session = auth_sign_up("mail@example.com", "password")
session.user["email"]   # => "mail@example.com"
auth_current_user()["uid"]
auth_sign_out()
auth_current_user()      # => nil
```

### AuthSession and Token Redaction

``auth_sign_up` / `auth_sign_in` return an AuthSession, a dedicated value that explicitly owns the user record and **ID / refresh tokens**. It is not a Map.

- Raw token strings are **never exposed to scripts**. They do not appear in `inspect`, `puts`, the final `fl` result, or `Debug` output. The display always has this form:
  ```
  AuthSession(user={"email" => "mail@example.com", "uid" => "...", ...}, id_token=<redacted>, refresh_token=<redacted>, expires_at=1769907600)
  ```
- Field access to a token returns a redacted projection (`<redacted:id_token>` / `<redacted:refresh_token>`). Scripts have no API to retrieve raw tokens.
- AuthSession values **cannot be written to documents**. If a `set` / `update` data map contains an AuthSession or token projection, it errors before reaching the backend. This prevents the in-memory backend from storing tokens.
- Raw tokens are held only in the Rust-side `AuthSession.id_token` / `refresh_token` (`AuthToken`) and can be retrieved explicitly with `AuthToken::expose()`. `Debug` / `Display` redact them.

### Session Lifetime and Sign-out

- A session has an ID token expiration (`expires_at`, Unix seconds), calculated from the emulator `expiresIn` response (accepted as either a string or number). Expired sessions make `auth_current_user()` return `nil`.
- `auth_sign_out()` discards the locally held session and tokens. It does **not revoke tokens on the server** (out of MVP scope). It is idempotent and does not error when already signed out.
- A new `auth_sign_up` / `auth_sign_in` replaces the existing session.

### Auth User Record Map Representation

`auth_current_user()` and `session.user` return the same user record as a Map. It may contain the following keys (normalized from Auth emulator field names):

| Key | Type | Description |
|------|----|------|
| `uid` | Str | Firebase user ID |
| `email` | Str | Canonicalized (lowercase) email |
| `emailVerified` | Bool | Always present (`false` if unverified) |
| `disabled` | Bool | Always present (`false` when enabled) |
| `displayName` | Str | Present only when available |
| `photoUrl` | Str | Present only when available |
| `createdAt` / `lastLoginAt` | Str | Present only when available |
| `providerId` | Str | Present only when available (for example, `"password"`) |

Password hashes and provider secrets are **not included**.

### Typed Errors

Auth backend failures have typed categories that are consistent across in-memory and emulator backends. Scripts receive them as ordinary backend errors: `error: <line>:<col>: <message>`.

| Category | Example trigger |
|----------|-----------|
| `UserNotFound` | `auth_sign_in` with an unknown email (emulator `EMAIL_NOT_FOUND`) |
| `InvalidCredentials` | Password mismatch (`INVALID_PASSWORD` / `INVALID_LOGIN_CREDENTIALS`) |
| `UserAlreadyExists` | `auth_sign_up` with an existing email (`EMAIL_EXISTS`) |
| `InvalidEmail` | Invalid email format (`INVALID_EMAIL`) |
| `WeakPassword` | Password shorter than six characters (`WEAK_PASSWORD`) |
| `Network` | Cannot connect to Auth emulator or timed out |
| `InvalidResponse` | Malformed emulator response or unknown error code |
| `Configuration` | Invalid host / project ID, or missing host |

Identity Toolkit error codes may include details after a colon (for example, `WEAK_PASSWORD : Password should be at least 6 characters`), so matching uses the leading code.

### Validation

- `auth_sign_up` / `auth_sign_in` validate that email and password are **strings** (email must include `@` and `.`, password must be at least six characters). Type and validation errors occur before reaching the backend.
- Emails are canonicalized (trimmed and lowercased) before lookup.
- `auth_current_user` / `auth_sign_out` take no arguments, keyword arguments, or blocks.
- Unknown keyword arguments, extra positional arguments, or a supplied block are errors.

## 12. Backends

| Entry point | `in-memory` | `emulator` feature |
|------------------|-------------|--------------------|
| `run_script` | Firestore and Storage are always in-memory | × |
| `run_script_with_firestore` | Selects Firestore; Storage is in-memory | ○ (pass `FirestoreEmulator`; Storage remains in-memory) |
| `run_script_with_storage` | Firestore is in-memory; selects Storage | Storage can be selected (Firestore remains in-memory) |
| `run_script_with_backends` | Selects Firestore and Storage separately | Same |
| `run_script_with_auth` | Selects Auth (Firestore / Storage are in-memory) | ○ (pass `AuthEmulator`) |
| `run_script_with_all_backends` | Selects all three backends separately | Same |
| `fl` CLI | ○ (default) | Firestore uses `--backend emulator`, Storage uses `--storage-backend`, Auth uses `--auth-backend emulator` |

### Build Features and Public API

- The default feature set is empty. `cargo build` / `cargo test` build only the in-memory backend and library API; they do not link Firestore SDK, gRPC, or TLS dependencies.
- Enabling the `emulator` feature enables the Firestore and Auth emulator clients: `cargo build --features emulator`. The dependency-free Storage REST client is available without that feature.
- `Firestore`, `InMemoryFirestore`, `run_script`, and `run_script_with_firestore` are available without features, so consumers embedding a custom in-memory backend do not pull in the SDK.
- `FirestoreEmulator` and `AuthEmulator` are not exposed in builds without the feature. `StorageEmulator` remains available because its synchronous HTTP transport uses only the standard library.
- Future clients should also be split into service-specific feature groups, following the emulator pattern.

- Authenticated access to **production Firestore is unsupported** and out of scope.
- The runtime abstracts backends through the `Firestore` and `Auth` traits. Defaults are `InMemoryFirestore` and `InMemoryAuth`.
- The Firestore emulator backend calls the Firestore API through the gRPC client in the Firestore Rust SDK (`firestore` crate). The database is fixed to `(default)`.
- The Auth emulator backend calls the local Identity Toolkit REST API (`accounts:signUp` / `accounts:signInWithPassword`) over HTTP. It does not use the production Firebase SDK and cannot implicitly reach production credentials.
  - Public endpoints authenticate with an API key query (`?key=...`). **Do not send an OAuth2 `Authorization` header**: the emulator treats it as an Admin SDK call and does not issue tokens in sign-up responses.
  - Set `FIREBASE_API_KEY` to override the API key (default: the emulator convention `fake-api-key`).
- Both Firestore backends must have the same observable behavior (value round-trips, query ordering, and batch/transaction atomicity). Storage backends share path, metadata, content type, size, and not-found contracts. Auth backends share return-value and typed-error contracts.
- Storage backends implement the `Storage` trait. The in-memory backend stores `Vec<u8>`; the emulator calls the Firebase Storage REST API (`/v0/b/{bucket}/o`) over synchronous HTTP. The bucket defaults to `{project_id}.appspot.com` and can be overridden with `FIREBASE_STORAGE_BUCKET`. HTTP I/O is isolated in `StorageTransport`, allowing contract tests with a fake transport and no network.
- When emulator documents contain fields written using types outside flare (`double` / `reference` / `geo_point`), the emulator backend returns them as `Opaque` / `DocRef` (§3). `bytes` round-trip losslessly as `Bytes`. The in-memory backend cannot contain values of types outside flare.
- Local runs use `firestore.emulator.rules` (allowing reads and writes), referenced by `firebase.emulators.json`. `firestore.rules`, referenced by `firebase.json`, denies reads and writes to guard against accidental deployment to a remote project ([separating Firebase rules and preventing deployment](firebase-security.md)).

## 13. Execution Budgets and Trust Boundaries

`fl` accepts arbitrary script files. Malformed scripts, infinite recursion or loops, deep nesting, and huge query results must not exhaust the stack or hang indefinitely. The runtime has **finite execution budgets** by default and reports a normal positioned error (`error: <line>:<col>: ...`) when a budget is exceeded, rather than panicking.

Budgets reset for each `run` / `eval_program` call (they accumulate during that call).

| Budget | Default | Scope / notes |
|------|--------|-----------|
| Call depth | 32 | Nested calls (user functions and built-ins); limits recursion depth |
| Steps | 1,000,000 | Number of evaluated statements; `step limit exceeded` |
| Loop iterations | 100,000 | Iterations of `while` / `each` / `map`, shared across the run; `loop iteration limit exceeded` |
| Wall-clock time | 5,000 ms | Time spent lexing, parsing, and evaluating; `wall-clock limit exceeded` |
| Parser depth | 128 | Nesting of parentheses, Map/Array literals, `else if` chains, and `!` / `-`; `parser nesting limit exceeded` |
| Interpolation depth | 32 | Nested `"#{...}"` |
| Value nesting depth | 64 | Nesting of runtime `Value` arrays / maps. The root is depth 0, and the limit **includes that depth** (depth 64 is allowed; a child beyond it exceeds the limit). Childless values do not exceed the limit, so scalars and empty arrays/maps pass with a limit of 0. `value nesting limit exceeded`. The emulator `Value` ⇔ SDK codec uses the same recursion limit so deeply nested stored documents cannot exhaust the stack **before** runtime validation. |
| Value nodes | 100,000 | Total nodes in runtime `Value`s. The root counts as one, so a limit of 0 rejects even scalar results. Nodes are counted **when children are added**, before a huge backend array is fully placed on the stack. `value node limit exceeded` |
| String size | 10 MiB | Maximum byte size of a single `Value::Str`; `string size limit exceeded` |
| Bytes size | 50 MiB | Maximum byte size of a single `Value::Bytes`; `bytes size limit exceeded` |
| Query results | 10,000 | Matching candidates scanned by one query; `query result limit exceeded` |
| Source length | 1 MiB | UTF-8 bytes |
| Tokens | 100,000 | Lexical tokens, including interpolation |
| Backend timeout | 5,000 ms | Per backend operation. Firestore and Auth enforce full operations; Storage applies one deadline across socket I/O but reports support as false because synchronous DNS is not bounded. |

- All budget errors use `ErrorKind::Runtime` (or `Lex` / `Parse` when caused by lexing or parsing) and point to the start of the failed top-level statement. A wall-clock limit exceeded before evaluation starts points to `1:1`.
- A budget error aborts `batch` without applying any writes and aborts a `transaction`, preserving the same atomicity as an ordinary runtime error. A budget error after commit is not reported as failure after successful persistent writes.
- **Call depth protects the native stack.** A user function call uses about 50 KiB of stack in debug builds, including argument and receiver contract validation. On a host with a 2 MiB stack (such as a libtest thread), measurements show stack exhaustion at 40 levels, so the default limit of 32 stops beforehand. Hosts with larger stacks may raise `max_call_depth`; do not disable the budget on stacks smaller than 2 MiB, where deep recursion may cause a process abort from stack overflow.
- An AST passed by an embedding application to `eval_program` bypasses the parser, so the same depth budget is checked before entering `eval_block`. The error is `Parse` and points to the start of the program, `1:1`.
- The query-result limit applies to the **matching candidates scanned**. The backend stops when candidates exceed the budget (without materializing the one candidate that proves the excess), then applies `order_by` / `offset` / `limit` to produce the page for the script. Thus `limit(3)` succeeds with 20 candidates if they are within budget and returns three items.
- The string-size and bytes-size limits apply to runtime `Value::Str` and `Value::Bytes` instances created via concatenation, string interpolation, `bytes(...)` conversions, or `storage_download()`. If a single string or byte sequence exceeds its budget, evaluation stops immediately with `string size limit exceeded` or `bytes size limit exceeded`.

### Embedding API

Budgets can be configured with `RuntimeLimits`. Finite defaults apply to **both** library embedding and the `fl` CLI.

```rust
let limits = RuntimeLimits::default().with_max_call_depth(32);
let mut rt = Runtime::with_limits(backend, limits);
let out = rt.run(source)?;
let usage = rt.usage();   // steps / loop_iterations / max_call_depth / ...
```

Each budget can be overridden with `RuntimeLimits` builders such as `with_max_steps`, `with_max_loop_iterations`, `with_max_wall_clock`, `with_max_parser_depth`, `with_max_value_depth`, `with_max_string_bytes`, `with_max_bytes_size`, and `with_max_query_results`. `Runtime::set_limits` can change limits on a running runtime.

### Disabling Budgets (Trusted Scripts)

`Runtime::new_unlimited` / `RuntimeLimits::unlimited()` are an **explicit opt-in** that disables all budgets. Use them only with trusted scripts, never with user input, uploaded scripts, or the CLI. `fl --unlimited` is the CLI equivalent.

The generic synchronous `Firestore` trait cannot forcibly interrupt arbitrary implementations, so `backend_timeout` is enforced by the built-in `FirestoreEmulator` (check with `Runtime::backend_timeout_supported()`). Custom backends must implement their own timeout. Backend-call timeouts are reported as `ErrorKind::Backend` to preserve the cause and point to the start of the failed statement.

## 14. Out of Scope (MVP)

The following are explicitly **unsupported**. Do not implement them or imply support in the README.

- Float (numbers are Int only)
- Authenticated access to production Firestore
- Authenticated access to production Cloud Storage (automatic ID token acquisition, OAuth, etc.). The emulator client accepts an explicit bearer token option for testing; without one, requests are unauthenticated and governed by local rules.
- **Connecting to production Firebase Authentication** (`AuthEmulator` supports only the local emulator)
- Server-side token revocation on Auth sign-out
- ID token refresh (there is no `auth_refresh`; call `auth_sign_in` after expiration)
- Sign-in methods other than the Auth provider (OAuth / anonymous / MFA)
- Firestore rules evaluation through Auth (attaching a token to Firestore requests)
- Automatic transaction retries
- Cursor-based pagination (`start_after`, etc.; use `order_by` + `offset` + `limit` instead)
- Listeners (real-time subscriptions) and index-dependent features
- mruby compatibility beyond the subset described in this document

## 15. Known Limitations and Implementation Notes

## 16. Cloud Storage API

The Cloud Storage MVP CRUD API is provided as global functions and uses object paths separate from Firestore `doc()` / `col()` paths.

### 16.1 Values and Paths

- `bytes([0, 255, 128])` creates `Value::Bytes`. Array elements must be Int values in `0..=255`. Strings are not implicitly converted to UTF-8, so arbitrary downloaded bytes can be compared with `storage_download()`.
- A Storage object path consists of one or more non-empty segments joined by `/`; `"avatars/alice.png"` and `"a/b/c"` are valid. Firestore document even/odd segment rules do not apply. Leading or trailing `/`, `//`, an empty path, or NUL/control characters cause `invalid storage object path`.
- Paths and content types are validated identically by the in-memory and emulator backends. The default content type is `application/octet-stream`.

### 16.2 CRUD

| Function | Return value | Description |
|------|--------|------|
| `storage_upload(path, bytes_value, content_type: "image/png")` | Metadata Map | Creates or replaces an object. Content type can also be positional, but positional and keyword values cannot both be supplied. The returned `name`, `bucket`, `content_type`, `size`, etc. can be inspected. |
| `storage_download(path)` | Bytes | Returns the object bytes unchanged; the HTTP body is not converted to UTF-8. |
| `storage_metadata(path)` | Metadata Map | Returns object metadata, including `generation`, `metageneration`, `size`, and `content_type`. |
| `storage_delete(path)` | Nil | Deletes the object. A missing object returns `storage object not found`. |

Metadata Map keys use snake_case (`name`, `bucket`, `content_type`, `size`, `generation`, `metageneration`, `time_created`, `updated`, `md5_hash`, `crc32c`, `etag`, `cache_control`, `content_disposition`, `metadata`). Optional values absent from the server response are `nil`. The emulator adapter also verifies that `size` matches the uploaded byte count. Storage APIs are not staged by Firestore `batch` / `transaction`; calling them inside either produces `storage APIs are not allowed inside ...`.

### 16.3 Errors and Authentication

- Invalid paths produce `invalid storage object path: ...`; invalid MIME types produce `invalid storage content type: ...`; malformed JSON produces `invalid storage metadata ...`; and size mismatches produce `storage metadata size mismatch ...`.
- A 404 from download / metadata / delete is normalized across backends to `storage object not found: <path>`.
- `StorageEmulator` is created from `FIREBASE_STORAGE_EMULATOR_HOST` and a project ID. Without an explicit authorization header, requests are unauthenticated and governed by Storage emulator rules. The Rust client’s `with_auth_token()` or CLI’s `FIREBASE_STORAGE_AUTH_TOKEN` adds a bearer header for rules testing; production authentication and automatic token acquisition are out of MVP scope.

### 16.4 Local Quickstart

```bash
export FIREBASE_PROJECT_ID=demo-flare
firebase emulators:exec --only storage --project demo-flare \
  'cargo test --test storage_emulator -- --nocapture'
```

`storage.rules` in `firebase.json` allows all objects as a local CRUD fixture. Production rules, listing, resumable uploads, public URLs, object generation operations, and authenticated production access are separate concerns.

There are currently no unresolved limitations in §7 `where` comparisons across types (including the handling of Nil, Array, Map, and missing fields). Even when the emulator uses Firestore server filters to narrow candidates, the shared client-side matcher in §7 makes the final decision.
| Item | Current behavior | Planned |
|------|-----------|------|
| Auth session persistence | Sessions are held only in process memory and are not persisted between script runs. | backlog |
