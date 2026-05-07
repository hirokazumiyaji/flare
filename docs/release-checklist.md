# MVP release checklist

Use before tagging a release or publishing binaries.

1. **Local**
   - `cargo fmt --check`
   - `cargo test --all-targets`
   - `cargo build --release --bin fl`

2. **Firestore emulator (optional but recommended)**
   - Install [Firebase CLI](https://firebase.google.com/docs/cli) and Java 11+.
   - From the repo root:  
     `firebase emulators:exec --only firestore --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator -- --nocapture'`
   - Same command runs in CI (`.github/workflows/ci.yml`, job `integration-emulator`).

3. **Smoke**
   - `./target/release/fl --help`
   - `./target/release/fl examples/scripts/01_basic.fl`
   - With emulator: set `FIRESTORE_EMULATOR_HOST` and `FIRESTORE_PROJECT_ID`, then `./target/release/fl --backend emulator examples/scripts/01_basic.fl`

4. **CD**
   - Push a `v*` tag to trigger `.github/workflows/cd.yml` (if enabled).
