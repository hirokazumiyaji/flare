# Task 06: Test Matrix And CI Coverage

## Status

Done

## Goal

Make the supported MVP behavior mechanically provable instead of relying on examples and smoke tests.

## Why This Task Exists

The current tests prove that the in-memory runtime works and that one emulator write path exists, but they do not protect the real MVP surface.

## Deliverables

- A clear split between unit tests and emulator integration tests.
- Coverage for CRUD, merge, query, collection group, sentinels, batch, and transaction.
- A CI path that exercises emulator-backed tests.

## Test Layers

- Unit tests for lexer, parser, runtime, and in-memory backend behavior.
- Emulator integration tests for end-to-end Firestore behavior.
- CLI smoke tests for backend selection and user-facing execution.

## Work Items

- Expand emulator integration tests beyond the current smoke test.
- Add deterministic query repeat tests.
- Add merge behavior tests.
- Add nested array and map round-trip tests.
- Add `server_timestamp()` tests.
- Add `batch` atomicity tests.
- Add `transaction` atomicity and read-then-write tests.
- Add `transaction` read-before-write restriction tests.
- Add transaction tests with multiple `get` calls before the first staged write.
- Add one CLI test for `--backend emulator`.
- Add one CLI usage test for help or invalid backend invocation.
- Update CI to provision and run the Firestore emulator for integration tests.
- Keep emulator tests skippable locally by following the current env-check pattern when `FIRESTORE_EMULATOR_HOST` is unset. CI should always provide the emulator environment.

## Acceptance Criteria

- The supported README API surface has at least one integration test each.
- Query determinism is enforced by tests.
- `batch` and `transaction` rollback semantics are enforced by tests.
- CI runs emulator-backed integration tests in a documented way.
- CLI emulator execution is covered by at least one smoke test.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).
- Depends on [Task 02](02-cli-backend-selection.md).
- Depends on [Task 03](03-emulator-crud-and-codec.md).
- Depends on [Task 04](04-query-and-determinism.md).
- Depends on [Task 05](05-batch-and-transaction.md).

## Notes

If emulator startup time makes CI noisy, split the workflow into a fast unit job and a slower integration job instead of weakening coverage.
