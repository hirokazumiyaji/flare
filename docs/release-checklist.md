# MVP release checklist

Use before tagging a release or publishing binaries.

## 0. Pinned tools

- `rust-toolchain.toml` pins Rust to **1.98.1** and installs `rustfmt` and `clippy`. Check the active toolchain with `rustup show active-toolchain` and `rustc --version --verbose` before building.
- CI and CD use the same `rust-toolchain.toml` and the checked-in `Cargo.lock`. Dependency-resolving Cargo commands must include `--locked`.
- The Firestore emulator job uses Node **24.11.0**, Temurin Java **21.0.8**, and Firebase CLI **15.31.0**. For a local emulator run, install the same CLI explicitly and verify it:

  ```bash
  npm install --global firebase-tools@15.31.0
  firebase --version  # must print 15.31.0
  ```

- GitHub Actions are pinned to full commit SHAs in the workflow comments. `.github/dependabot.yml` keeps action updates separate from Cargo dependency updates; review each update independently. Rust and Firebase CLI version changes are deliberate tool-update changes and must not be mixed with a `Cargo.lock` dependency update.

## 1. Local

- `cargo metadata --locked --no-deps --format-version 1`
- `cargo fmt --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --all-targets`
- `cargo build --locked --release --bin fl`

## 2. Firestore emulator (optional but recommended)

- Install [Firebase CLI](https://firebase.google.com/docs/cli) 15.31.0 and Java 21.0.8.
- From the repo root:

  ```bash
  firebase emulators:exec --only firestore --project demo-firelang \
    'FIRESTORE_PROJECT_ID=demo-firelang cargo test --locked --test firestore_emulator -- --nocapture'
  ```

- The same command runs in CI (`.github/workflows/ci.yml`, job `integration-emulator`). The job prints Rust, Node, npm, Java, and Firebase CLI versions before starting the emulator.

## 3. Smoke

- `./target/release/fl --help`
- `./target/release/fl examples/scripts/01_basic.fl`
- With emulator: set `FIRESTORE_EMULATOR_HOST` and `FIRESTORE_PROJECT_ID`, then `./target/release/fl --backend emulator examples/scripts/01_basic.fl`

## 4. CD

- Push a `v*` tag to trigger `.github/workflows/cd.yml` (if enabled).
- The release workflow builds the `x86_64-unknown-linux-gnu` binary with Cargo's lockfile enforced and attaches `tool-versions.txt`. Check that record for the Rust/Cargo versions, lockfile hash, runner image, and the Firebase CLI version used by CI.
