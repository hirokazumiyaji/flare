# Firebase rules の分離とデプロイ防止策

firelang は認証付きの本番 Firestore と Cloud Storage に対応していません。
そのため、リポジトリの通常の Firebase デプロイ先では、すべての read と write を拒否します。
許可ポリシーを実装するまでは、誤ってデプロイしてもデータを公開できない状態を維持します。

## 設定の分離

`firebase deploy` と `firebase emulators:*` は、通常 `--config` を省略すると `firebase.json` を読みます。
rules の参照先も、この設定で決まります。
このため、deploy 用と emulator 用の config と rules を別のファイルに分けます。

| 対象 | リモート project 用の設定 | ローカル emulator 用の設定 |
| --- | --- | --- |
| Firestore | `firebase.json` → `firestore.rules`（read/write 拒否） | `firebase.emulators.json` → `firestore.emulator.rules`（read/write 許可） |
| Cloud Storage | `firebase.json` → `storage.rules`（read/write 拒否） | `firebase.emulators.json` → `storage.emulator.rules`（read/write 許可） |

`firebase deploy` を `--config` なしで実行すると、`firebase.json` が選択されます。
したがって、明示的に production-like project を指定しても、rules は read/write 拒否の状態でデプロイされます。

read/write を許可する emulator rules は `firebase.emulators.json` からだけ読み込みます。
`firebase deploy --config firebase.emulators.json` は実行しないでください。
このコマンドは許可 rules をリモート project へ送るため、テストや CI の deployment にも使いません。

## ローカルエミュレータの project

ローカルエミュレータでは `demo-firelang` を明示します。
`demo-` で始まる project ID を使い、実運用の project ID をローカル設定へ追加しません。

```bash
firebase emulators:start \
  --config firebase.emulators.json \
  --only firestore \
  --project demo-firelang
```

Firestore の統合テストも同じ設定を使います。

```bash
firebase emulators:exec \
  --config firebase.emulators.json \
  --only firestore \
  --project demo-firelang \
  'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'
```

リポジトリには `.firebaserc` を用意していません。
clone した直後は既定 project が未選択です。
このドキュメントのコマンドは project ID を明示します。

## Authentication と Storage の扱い

現在の emulator config は Authentication を起動しません。
ランタイムも本番の認証情報を扱わないため、本番用 rules は要求ごとに拒否するだけにしています。

Authentication のサポートを追加するときは、読み書き要件ごとの security policy を実装します。
policy の追加では、匿名リクエストと権限のない認証済みリクエストを拒否し、所有者と管理者のリクエストだけを許可してください。
Firestore と Storage の両方で、許可する操作と拒否する操作を `demo-firelang` の emulator に対する integration test に含めます。

Cloud Storage は現在のランタイム API には含まれません。
`storage.emulator.rules` は将来のローカル integration test 用であり、`demo-firelang` 以外への deploy には使いません。
一方、`firebase.json` は `storage.rules` を参照するため、Storage を誤って deploy しても read/write は拒否されます。

## Deployment guard のテスト

次のテストは、設定と rules の分離、および production 側の全拒否ポリシーを検査します。

```bash
cargo test --test firebase_config
```

CI では `firebase.json` 自身を使って Firestore emulator を起動し、write が `PERMISSION_DENIED` になることを検査します。
emulator の `Bearer owner` は admin 資格で rules をバイパスするため、この検査は admin でない資格(`EmulatorAuth::EnforceRules`)で行います。
静的テストも production 側の rules が read/write 拒否のままであることを検査するため、通常の `firebase deploy` によって production-like project が公開されることはありません。
