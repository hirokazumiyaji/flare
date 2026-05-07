# Task 02: CLI Backend Selection And Emulator Wiring

## Status

Done

## Goal

Make the `fl` binary capable of executing scripts against either the in-memory backend or the Firestore emulator.

## Why This Task Exists

Today the CLI always uses `InMemoryFirestore`, so the repository cannot yet demonstrate its Firestore API from the user-facing entrypoint.

## Deliverables

- A backend selection path in `fl`.
- A reusable emulator backend in `src/`, not only in tests.
- A documented environment and flag contract.
- Usage output that documents backend selection.

## Proposed CLI Contract

- `fl path/to/script.fl` keeps working and defaults to `in-memory`.
- `fl --backend emulator path/to/script.fl` runs against the emulator.
- `FIRESTORE_EMULATOR_HOST` and `FIRESTORE_PROJECT_ID` are honored when the emulator backend is selected.
- `fl --help` and invalid invocations print backend-aware usage.
- CLI exits with a clear error if emulator configuration is incomplete.

## Work Items

- Move emulator-specific backend code out of the test file into a runtime module.
- Keep manual argument parsing for MVP.
- Add backend selection to `src/main.rs`.
- Keep `run_script()` as the in-memory convenience path.
- Keep `run_script_with_firestore()` as the generic execution path.
- Add one CLI smoke test or scripted integration path for emulator execution.
- Document the invocation pattern in the README.

## Acceptance Criteria

- A user can run one script with `in-memory` and one script with `emulator` from the CLI.
- Backend choice is visible and predictable.
- The emulator backend code is no longer trapped inside `tests/firestore_emulator.rs`.
- Missing emulator config fails early with a clear message.
- Help and usage output show how to select the emulator backend.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).

## Risks

- Pulling emulator logic into production code without a clean module boundary will make Task 03 and Task 05 harder. Keep the backend behind the existing `Firestore` abstraction or a small extension of it.
