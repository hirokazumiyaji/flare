# MVP release checklist

Use before tagging a release or publishing binaries.

1. **Local**
   - `cargo fmt --check`
   - `cargo clippy --all-targets -- -D warnings`
   - `cargo test --all-targets`
   - `cargo build --release --bin fl`

2. **Firestore / Storage emulators (optional but recommended)**
   - Install [Firebase CLI](https://firebase.google.com/docs/cli) and Java 11+.
   - From the repo root:  
     `firebase emulators:exec --only firestore,storage --project demo-firelang 'FIRESTORE_PROJECT_ID=demo-firelang FIREBASE_PROJECT_ID=demo-firelang cargo test --test firestore_emulator --test storage_emulator -- --nocapture'`
   - Storage-only quickstart: `firebase emulators:exec --only storage --project demo-firelang 'cargo test --test storage_emulator -- --nocapture'`
   - Same combined command runs in CI (`.github/workflows/ci.yml`, job `integration-emulator`).

3. **Smoke**
   - `./target/release/fl --help`
   - `./target/release/fl examples/scripts/01_basic.fl`
   - With emulators: set `FIRESTORE_EMULATOR_HOST` / `FIREBASE_STORAGE_EMULATOR_HOST` and project IDs, then `./target/release/fl --backend emulator --storage-backend emulator examples/scripts/01_basic.fl`

4. **CD**
   - Push a `v*` tag to trigger `.github/workflows/cd.yml` (if enabled).
