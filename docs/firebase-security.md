# Firebase rules separation and deployment safeguards

flare does not support authenticated production Firestore or Cloud Storage. The repository's normal Firebase deployment target therefore denies all reads and writes. Until an allow policy is implemented, an accidental deployment must not expose data.

## Separate configurations

When `--config` is omitted, `firebase deploy` and `firebase emulators:*` normally read `firebase.json`. This file also determines which rules are used. Keep deployment and emulator configurations and rules in separate files:

| Target | Configuration for remote projects | Configuration for local emulators |
| --- | --- | --- |
| Firestore | `firebase.json` → `firestore.rules` (deny reads and writes) | `firebase.emulators.json` → `firestore.emulator.rules` (allow reads and writes) |
| Cloud Storage | `firebase.json` → `storage.rules` (deny reads and writes) | `firebase.emulators.json` → `storage.emulator.rules` (allow reads and writes) |

Running `firebase deploy` without `--config` selects `firebase.json`. Its rules deny reads and writes even when a production-like project is explicitly selected.

The emulator rules that allow reads and writes are loaded only through `firebase.emulators.json`. Do not run `firebase deploy --config firebase.emulators.json`. That command would upload the permissive rules to a remote project, so it must not be used for tests or CI deployment.

## Local emulator project

Always specify `demo-flare` for local emulators. Use project IDs beginning with `demo-`; do not add production project IDs to the local configuration.

```bash
firebase emulators:start \
  --config firebase.emulators.json \
  --only firestore \
  --project demo-flare
```

Firestore integration tests use the same configuration:

```bash
firebase emulators:exec \
  --config firebase.emulators.json \
  --only firestore \
  --project demo-flare \
  'FIRESTORE_PROJECT_ID=demo-flare cargo test --test firestore_emulator -- --nocapture'
```

The repository does not include a `.firebaserc`. No default project is selected after cloning, so the commands in this document specify the project ID explicitly.

## Authentication and Storage

The runtime supports Auth and Storage emulator APIs, but it does not forward Auth tokens to Firestore or Storage. The runtime also does not handle production credentials, so the production rules deny every request.

If Auth identity is later connected to Firestore or Storage, implement a security policy for each read and write requirement. The policy must deny anonymous requests and authenticated requests without permission, and allow only owner and administrator requests. Add integration tests for allowed and denied operations against the `demo-flare` Firestore and Storage emulators.

Cloud Storage is available through the runtime, but its emulator rules remain reserved for local integration tests and must not be deployed to any project other than `demo-flare`. `firebase.json` refers to `storage.rules`, so an accidental Storage deployment still denies reads and writes.

## Deployment guard tests

The following test checks the separation between configurations and rules, as well as the production-side deny-all policies:

```bash
cargo test --test firebase_config
```

CI starts the Firestore emulator using `firebase.json` itself and verifies that writes return `PERMISSION_DENIED`. The emulator's `Bearer owner` bypasses rules with admin credentials, so this check uses non-admin credentials (`EmulatorAuth::EnforceRules`). Static tests also verify that the production rules continue to deny reads and writes. A normal `firebase deploy` therefore cannot expose a production-like project.
