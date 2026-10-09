# flare

A Rust implementation of an **mruby-inspired scripting language runtime** for working with Firestore and Cloud Storage.

```ruby
set("users/1", {name: "alice", age: 20})
rows = col("users").where("age", ">=", 20).limit(10).get()
puts "found #{rows}"

payload = bytes([0, 255, 128])
storage_upload("avatars/alice.bin", payload, content_type: "application/octet-stream")
content = storage_download("avatars/alice.bin")
```

The runtime also provides an MVP authentication API for the local Firebase Auth Emulator: `auth_sign_up`, `auth_sign_in`, `auth_current_user`, and `auth_sign_out`.

The complete language and API contract—including integer-only numbers, immutable variables, and atomic `batch` / `transaction` operations—is documented in the **[Language Specification / API Reference](docs/spec.md)**, the repository's sole authoritative specification.

## Documentation

- [Language Specification / API Reference](docs/spec.md): language, Firestore and Storage APIs, batch/transaction semantics, and Auth API
- [examples/scripts/](examples/scripts/): runnable example scripts
- [Firebase rules separation and deployment safeguards](docs/firebase-security.md)
- [Release checklist](docs/release-checklist.md)
- [Archived MVP planning documents](docs/archive/mvp/)

## Reproducible builds

Rust is pinned to **1.98.1** in [`rust-toolchain.toml`](rust-toolchain.toml).
Unless you are updating dependencies, pass `--locked` to Cargo commands that resolve dependencies. CI and CD use the same toolchain and `Cargo.lock`, and pin the runner image to `ubuntu-24.04`.

Firestore emulator CI uses Node **24.11.0**, Temurin Java **21.0.8**, and Firebase CLI **15.31.0**. The job logs each version. To use the same Firebase CLI locally, install it explicitly and check it with `firebase --version`:

```bash
npm install --global firebase-tools@15.31.0
firebase --version  # should print 15.31.0
```

GitHub Actions are pinned to full commit SHAs. [Dependabot](.github/dependabot.yml) proposes Rust dependency updates and Action updates in separate pull requests.

## Usage

```bash
# Default build: in-memory only; the emulator SDK is not linked
cargo run --locked --bin fl -- path/to/script.fl
```

Enable the Firestore emulator backend explicitly with the `emulator` feature. `FIRESTORE_EMULATOR_HOST` is required; the default project ID is `flare-test`. The in-memory backend obtains the request time for `server_timestamp()` at most once per commit. Rust callers can inject a deterministic clock with `InMemoryFirestore::with_clock(...)`:

```bash
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
export FIRESTORE_PROJECT_ID=flare-test
cargo run --locked --features emulator --bin fl -- --backend emulator path/to/script.fl
```

Start the emulator with the Firebase CLI. Use the explicitly local-only `firebase.emulators.json` config, which allows all rules, and the `demo-flare` project ID:

```bash
firebase emulators:start --config firebase.emulators.json --only firestore --project demo-flare
```

To run the library and CLI integration tests twice against the same emulator, use the following. Fixtures from the first pass remain in place during the second pass, which helps detect interference between tests:

```bash
firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-flare \
  'set -eu
    env | grep -q "^FIRESTORE_EMULATOR_HOST="
    for run in 1 2; do
      FIRESTORE_PROJECT_ID=demo-flare cargo test --features emulator --test firestore_emulator --test cli --locked -- --nocapture
    done'
```

`firebase.json` is for deployment and denies Firestore and Storage reads and writes. Do not run `firebase deploy --config firebase.emulators.json`. See [Firebase rules separation and deployment safeguards](docs/firebase-security.md) for details.

When run without the feature, `fl --backend emulator` exits without loading the SDK and explains how to enable the `emulator` feature. Consumers that only need in-memory support can use the same API with `cargo build --no-default-features`.

You can also start the emulator in another terminal (for example, with `firebase emulators:start --only firestore`) and run a one-shot test using the repository's `firebase.json`:

```bash
firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-flare \
  'FIRESTORE_PROJECT_ID=demo-flare cargo test --features emulator --test firestore_emulator --locked -- --nocapture'
```

Production Firestore and authenticated production Cloud Storage are not supported. See [Backends](docs/spec.md#12-backends) and [Cloud Storage API](docs/spec.md#16-cloud-storage-api) in the specification.

## Cloud Storage

The MVP Storage CRUD API consists of the global functions `storage_upload`, `storage_download`, `storage_metadata`, and `storage_delete`. `bytes([0, 255])` is a byte sequence, not a UTF-8 string, and downloads are not converted to text. Unlike Firestore document paths, Storage object paths do not require an even or odd number of segments.

`firebase.json` configures local `storage.rules` and the emulator on port 9199. To test Storage by itself:

```bash
export FIREBASE_PROJECT_ID=demo-flare
firebase emulators:exec --config firebase.emulators.json --only storage --project demo-flare \
  'cargo test --features emulator --test storage_emulator --locked -- --nocapture'
```

To use Firestore and Storage together, pass `--only firestore,storage`. `fl --storage-backend emulator` requires `FIREBASE_STORAGE_EMULATOR_HOST`. With the default `auto` setting, `fl` uses the Storage emulator when a host is set and otherwise uses in-memory storage. The bucket name is derived from `FIREBASE_PROJECT_ID`, or from `GCLOUD_PROJECT`, `FIRESTORE_PROJECT_ID`, and finally `flare-test`. You can override it with `FIREBASE_STORAGE_BUCKET`. For local rules tests, you may also set `FIREBASE_STORAGE_AUTH_TOKEN` explicitly. Automatic production token acquisition is outside the MVP scope.

To connect to the Auth emulator, set `FIREBASE_AUTH_EMULATOR_HOST`:

```bash
export FIREBASE_AUTH_EMULATOR_HOST=127.0.0.1:9099
export FIREBASE_PROJECT_ID=flare-test
cargo run --locked --features emulator --bin fl -- --auth-backend emulator path/to/script.fl
```

`--backend` and `--auth-backend` are independent. Setting both to `emulator` connects to both emulators. **Tokens are never shown in `inspect`, `puts`, or `fl` output** (`AuthSession(user={...}, id_token=<redacted>, refresh_token=<redacted>, expires_at=...)`).

Start the emulator with the Firebase CLI in another terminal (for example, with `firebase emulators:start`). To run the tests once using the repository's `firebase.json`:

```bash
firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-flare \
  'FIRESTORE_PROJECT_ID=demo-flare cargo test --features emulator --test firestore_emulator --locked -- --nocapture'

firebase emulators:exec --config firebase.emulators.json --only auth --project demo-flare \
  'FIREBASE_PROJECT_ID=demo-flare cargo test --features emulator --test auth_emulator --locked -- --nocapture'
```

Connections to production Firestore and production Firebase Authentication are not supported. See [Backends](docs/spec.md#12-backends) in the specification.

## Trust boundaries and execution budgets

`fl` accepts arbitrary files as scripts. To prevent infinite recursion, infinite loops, deep nesting, and huge query results from exhausting the stack or running indefinitely, the runtime has **finite execution budgets by default**. Exceeding a budget returns a regular error such as `error: <line>:<col>: ...`; it does not panic or get killed by the OS.

The defaults apply both to library embedding and the `fl` CLI.

| Budget | Default | Applies to |
|--------|---------|------------|
| Call depth | 32 | Nested calls (user functions and built-ins) |
| Steps | 1,000,000 | Number of evaluated statements |
| Loop iterations | 100,000 | `while`, `each`, and `map` iterations, shared across the whole run |
| Wall-clock time | 5,000 ms | Time spent lexing, parsing, and evaluating |
| Parser depth | 128 | Nesting of parentheses, literals, `else if` chains, and similar constructs |
| Interpolation depth | 32 | Nested `"#{...}"` expressions |
| Value nesting depth | 64 | Nesting of runtime `Value`s (root depth is 0; the limit is inclusive) |
| Value nodes | 100,000 | Total runtime `Value` nodes (the root counts as one node) |
| Query results | 10,000 | Matching candidates scanned by one query (`limit` / `offset` apply afterward) |
| Source length | 1 MiB | Number of UTF-8 bytes |
| Tokens | 100,000 | Number of tokens, including those inside interpolation |
| Backend timeout | 5,000 ms | One Firestore call (enforced by the built-in emulator backend) |

Set budgets through the embedding API:

```rust
use std::time::Duration;
use flare::{InMemoryFirestore, Runtime, RuntimeLimits};

let limits = RuntimeLimits::default()
    .with_max_call_depth(32)
    .with_max_wall_clock(Duration::from_millis(500));

let mut rt = Runtime::with_limits(InMemoryFirestore::default(), limits);
let out = rt.run("col(\"users\").limit(10).get()")?;

// Usage metrics can be inspected in tests.
let usage = rt.usage();
println!("steps={} loops={} max_depth={}", usage.steps, usage.loop_iterations, usage.max_call_depth);
```

`Runtime::new_unlimited` / `RuntimeLimits::unlimited()` are **explicit opt-ins for trusted scripts**. Do not use them for user input, uploaded files, or the CLI; keep the defaults in those cases.

> **Stack safety:** The call-depth budget protects Rust's native stack. A single user-function call uses about 50 KiB of stack in a debug build, so a 2 MiB stack (such as a test thread) is exhausted at around 40 levels. The default of 32 stops before that point, making it safe for hosts with small stacks. To allow deeper recursion, raise the limit with `with_max_call_depth` only on hosts with more than 2 MiB of stack. Disabling the budget can make deep recursion overflow the stack and abort the process.

The generic synchronous `Firestore` trait cannot forcibly interrupt an unresponsive backend, such as one waiting on a network. Therefore, only the built-in `FirestoreEmulator` enforces timeouts.

The CLI lets you override each budget individually; see `fl --help` for the full list:

```bash
fl --max-call-depth 32 --max-steps 100000 script.fl
fl --max-query-results 100 -e 'col("users").get()'
```

## Tests

```bash
cargo test --locked --all-targets            # unit and integration tests
cargo test --locked --test budgets           # execution budget tests (issue #82)
cargo test --locked --test example_scripts   # run all example scripts
cargo test --locked --test cli               # CLI smoke tests
cargo test --locked --test release           # release tag/package validation
cargo test --locked --test auth              # in-memory Auth contract tests
cargo test --locked --test firebase_config   # Firebase deployment guard

# Default features: in-memory only
cargo check --locked --no-default-features --all-targets
cargo test --locked --no-default-features --all-targets

# Build with the emulator SDK enabled
cargo check --locked --features emulator --all-targets
cargo test --locked --features emulator --all-targets
```

The emulator integration tests (`firestore_emulator`, `storage_emulator`, and `auth_emulator`) return early and skip when their corresponding emulator host variable is not set. To run them, enable the `emulator` feature and use `firebase emulators:exec` as above, or set the environment variables after starting the emulators:

```bash
cargo test --locked --features emulator --test firestore_emulator -- --nocapture
cargo test --locked --features emulator --test storage_emulator -- --nocapture   # skipped if FIREBASE_STORAGE_EMULATOR_HOST is unset
cargo test --locked --features emulator --test auth_emulator -- --nocapture      # skipped if FIREBASE_AUTH_EMULATOR_HOST is unset
```

When connected to a persistent emulator, rerunning a command or splitting integration test binaries does not cause contention. Each test writes to its own `_flare_tests/<test-name>_<unique-run>/docs` hierarchy. If an environment variable is set but the connection fails, the test fails instead of skipping.

`Firestore`, `InMemoryFirestore`, and `run_script` are available without features. `FirestoreEmulator` and `fl --backend emulator` are available only with the `emulator` feature. Consumers that use the emulator backend as a library must specify `features = ["emulator"]` in their dependency declaration.

## CI / CD (GitHub Actions)

- **`test` job:** runs `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --all-targets --locked`, and `cargo build --release --bin fl --locked` with the pinned Rust toolchain and `Cargo.lock`; logs tool versions.
- **`feature-builds` job:** checks the default, `--no-default-features`, and `--features emulator` build boundaries.
- **`integration-emulator` job:** starts the Firestore, Storage, and Auth emulators with Firebase CLI 15.31.0, logs tool versions, and runs each integration suite. It runs the Firestore emulator suite twice against the same emulator to detect test interference.
- CI uses a reusable workflow. CD for a `v*` tag runs the same jobs, including integration tests. The release job depends on this gate and the tag version check, so a tag cannot be published if branch CI was skipped.
- **CD:** builds and releases the Linux `fl` binary with `--features emulator` only when the tag without its `v` prefix (for example, `v0.1.0` → `0.1.0`) matches the `flare` package version from `cargo metadata --no-deps`.
- Release assets are `flare-<version>-linux-x86_64.tar.gz` archives with SHA-256 `.sha256` sidecars, not raw binaries. Each archive contains the executable `fl`, `README.md`, `LICENSE`, `docs/spec.md`, and a per-file `SHA256SUMS` manifest.

After downloading, place the archive and its sidecar in the same directory and verify them as follows:

```bash
sha256sum -c flare-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf flare-0.1.0-linux-x86_64.tar.gz
(cd flare-0.1.0-linux-x86_64 && sha256sum -c SHA256SUMS)
```

- Each release also includes `tool-versions.txt`, which records the Rust/Cargo versions, `Cargo.lock` SHA-256, runner image, and Firebase CLI version used by CI.
- The `integration-emulator` job also verifies that the deployment `firebase.json` denies writes.

## License

[MIT](LICENSE)
