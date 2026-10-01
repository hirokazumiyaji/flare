# MVP release checklist

Use before tagging a release or publishing binaries.

## 0. Pinned tools

- `rust-toolchain.toml` pins Rust to **1.98.1** and installs `rustfmt` and `clippy`. Check the active toolchain with `rustup show active-toolchain` and `rustc --version --verbose` before building.
- CI and CD use the same `rust-toolchain.toml`, the checked-in `Cargo.lock`, and the `ubuntu-24.04` runner image. Dependency-resolving Cargo commands must include `--locked`.
- The emulator jobs use Node **24.11.0**, Temurin Java **21.0.8**, and Firebase CLI **15.31.0**. For a local emulator run, install the same CLI explicitly and verify it:

  ```bash
  npm install --global firebase-tools@15.31.0
  firebase --version  # must print 15.31.0
  ```

- GitHub Actions are pinned to full commit SHAs in the workflow comments. `.github/dependabot.yml` keeps action updates separate from Cargo dependency updates; review each update independently. Rust and Firebase CLI version changes are deliberate tool-update changes and must not be mixed with a `Cargo.lock` dependency update.

1. **Local quality gates**
   - `cargo fmt --check`
   - `cargo clippy --all-targets --locked -- -D warnings`
   - `cargo clippy --features emulator --all-targets --locked -- -D warnings`
   - `cargo check --no-default-features --all-targets --locked`
   - `cargo check --features emulator --all-targets --locked`
   - `cargo test --all-targets --locked`
   - `cargo test --locked --test firebase_config`
   - `cargo build --release --features emulator --bin fl --locked`

   These are the same required checks run by the reusable CI workflow. A
   successful build by itself is not a release gate.

2. **Firestore / Storage emulators (required release gate)**
   - Install [Firebase CLI](https://firebase.google.com/docs/cli) 15.31.0 and Java 21.0.8.
   - Local runs use the explicit local-only config and the `demo-firelang`
     project. Never run `firebase deploy --config firebase.emulators.json`:
     `firebase.json` is the deploy config and denies every read/write.
   - Firestore:
     `firebase emulators:exec --config firebase.emulators.json --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator --locked -- --nocapture'`
   - Firestore + Storage:
     `firebase emulators:exec --config firebase.emulators.json --only firestore,storage --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang FIREBASE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator --test storage_emulator --locked -- --nocapture'`
   - Storage-only quickstart: `firebase emulators:exec --config firebase.emulators.json --only storage --project demo-firelang 'FIREBASE_STORAGE_EMULATOR_HOST=localhost:9199 cargo test --features emulator --test storage_emulator --locked -- --nocapture'`
   - The `integration-emulator` job runs this suite for branch/PR CI and is
     also run by CD. The release therefore does not omit emulator coverage.
   - That job prints the Rust, Node, npm, Java, and Firebase CLI versions before
     starting the emulator, so a runner image or tool update shows up in the log.

3. **Auth emulator (optional but recommended)**
   - From the repo root:  
     `firebase emulators:exec --config firebase.emulators.json --only auth --project demo-firelang 'FIREBASE_PROJECT_ID=demo-firelang cargo test --features emulator --test auth_emulator --locked -- --nocapture'`
   - Same command runs in CI (`.github/workflows/ci.yml`, job `integration-emulator`).

4. **Smoke**
   - `./target/release/fl --help`
   - `./target/release/fl examples/scripts/01_basic.fl`
   - With emulator: set `FIRESTORE_EMULATOR_HOST` and `FIRESTORE_PROJECT_ID` (plus `FIREBASE_STORAGE_EMULATOR_HOST` for storage), then run the feature-enabled binary with `./target/release/fl --backend emulator examples/scripts/01_basic.fl` (the release workflow builds with `--features emulator`).

5. **Tag and package**
   - Confirm the tag is `v<Cargo package version>` and not merely any `v*` tag. The validator compares it with `cargo metadata --no-deps --format-version 1`:
     `scripts/validate-release-tag.sh v0.1.0`
   - Push the tag. CD calls the reusable CI workflow, validates the version,
     and publishes only after all jobs succeed. There is no `continue-on-error`
     or release bypass.
   - The release archive contains the executable `fl` with mode `0755`,
     `README.md`, `LICENSE`, `docs/spec.md`, and `SHA256SUMS`. CD also
     publishes the archive's `.sha256` sidecar.

6. **Verify a downloaded release**
   - Put the `.tar.gz` and `.tar.gz.sha256` files in the same directory.
   - Run `sha256sum -c firelang-<version>-linux-x86_64.tar.gz.sha256`.
   - Extract it and run `sha256sum -c SHA256SUMS` from the extracted directory.
   - Read the attached `tool-versions.txt` for the Rust/Cargo versions, the
     `Cargo.lock` SHA-256, the runner image, and the Firebase CLI version CI uses.
   - Confirm `./fl --help` runs, then publish the GitHub Release only after the
     checks above pass.
   - With Auth emulator: set `FIREBASE_AUTH_EMULATOR_HOST` and `FIREBASE_PROJECT_ID`, then `./target/release/fl --auth-backend emulator examples/scripts/11_auth_smoke.fl`
   - Confirm no token appears in output: the run must print `id_token=<redacted>` / `refresh_token=<redacted>`, never a raw token.

