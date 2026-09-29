# MVP release checklist

Use before tagging a release or publishing binaries.

1. **Local quality gates**
   - `cargo fmt --check`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo clippy --features emulator --all-targets -- -D warnings`
   - `cargo check --no-default-features --all-targets`
   - `cargo check --features emulator --all-targets`
   - `cargo test --all-targets`
   - `cargo build --release --features emulator --bin fl`

   These are the same required checks run by the reusable CI workflow. A
   successful build by itself is not a release gate.

2. **Firestore emulator (required release gate)**
   - Install [Firebase CLI](https://firebase.google.com/docs/cli) and Java 11+.
   - From the repo root:  
     `firebase emulators:exec --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --features emulator --test firestore_emulator --locked -- --nocapture'`
   - The `integration-emulator` job runs this suite for branch/PR CI and is
     also run by CD. The release therefore does not omit emulator coverage.

3. **Smoke**
   - `./target/release/fl --help`
   - `./target/release/fl examples/scripts/01_basic.fl`
   - With emulator: set `FIRESTORE_EMULATOR_HOST` and `FIRESTORE_PROJECT_ID`, then run the feature-enabled binary with `./target/release/fl --backend emulator examples/scripts/01_basic.fl` (the release workflow builds with `--features emulator`).

4. **Tag and package**
   - Confirm the tag is `v<Cargo package version>` and not merely any `v*` tag. The validator compares it with `cargo metadata --no-deps --format-version 1`:
     `scripts/validate-release-tag.sh v0.1.0`
   - Push the tag. CD calls the reusable CI workflow, validates the version,
     and publishes only after all jobs succeed. There is no `continue-on-error`
     or release bypass.
   - The release archive contains the executable `fl` with mode `0755`,
     `README.md`, `LICENSE`, `docs/spec.md`, and `SHA256SUMS`. CD also
     publishes the archive's `.sha256` sidecar.

5. **Verify a downloaded release**
   - Put the `.tar.gz` and `.tar.gz.sha256` files in the same directory.
   - Run `sha256sum -c firelang-<version>-linux-x86_64.tar.gz.sha256`.
   - Extract it and run `sha256sum -c SHA256SUMS` from the extracted directory.
   - Confirm `./fl --help` runs, then publish the GitHub Release only after the
     checks above pass.
