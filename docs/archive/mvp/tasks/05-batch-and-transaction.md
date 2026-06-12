# Task 05: Atomic `batch` And `transaction`

## Status

Done

## Goal

Turn `batch` and `transaction` from syntax sugar into real atomic execution units for the supported MVP operations.

## Why This Task Exists

Right now both constructs only evaluate a block. They do not buffer writes, do not commit atomically, and do not roll back on failure.

## MVP Semantics To Implement

- `batch` is write-only.
- `batch` buffers `set`, `update`, and `delete`.
- `batch` applies all writes at the end or none on failure.
- `transaction` supports `get`, `set`, `update`, and `delete`.
- Multiple `get` calls are allowed before the first staged write, including repeated reads of the same document and reads of different documents.
- `transaction` buffers writes and commits all writes atomically at the end.
- `transaction` reads must happen before the first staged write.
- `get` after the first staged write fails clearly.
- `transaction` is single-attempt for MVP. Backend errors abort the whole transaction.
- Queries inside `transaction` are out of scope.

## Deliverables

- A runtime execution context for normal execution, batch execution, and transaction execution.
- A write operation model that can be staged before commit.
- In-memory atomic behavior for `batch` and `transaction`.
- Emulator-backed atomic behavior for `batch` and `transaction`.
- Failure tests that prove rollback semantics.

## Work Items

- Introduce an internal write operation type instead of mutating the backend immediately.
- Teach `set`, `update`, and `delete` to stage writes when inside `batch` or `transaction`.
- Reject `get` after the first staged write inside `transaction`.
- Reject unsupported operations inside `batch`.
- Implement transactional reads with a backend transaction identifier before the write phase.
- Implement the in-memory path with a cloned working snapshot and commit-at-end semantics.
- Implement the emulator path with atomic commit support.
- Add tests for successful commit, runtime failure, backend failure, and read-then-write behavior inside a transaction.

## Acceptance Criteria

- A successful `batch` writes all staged operations.
- A failing `batch` writes nothing.
- A successful `transaction` commits all writes atomically.
- A failing `transaction` leaves no writes behind.
- Runtime failures and backend failures both satisfy the same no-partial-write rule.
- `get` after the first staged write inside a transaction fails with a defined error.
- Unsupported operations inside `batch` fail clearly.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).
- Depends on [Task 02](02-cli-backend-selection.md).
- Depends on [Task 03](03-emulator-crud-and-codec.md).
- Depends on [Task 04](04-query-and-determinism.md).

## Notes

Keep the MVP contract narrow. Query execution inside `transaction` is not required for the first usable version.
Avoid a local read-after-write overlay in MVP. The simpler contract is read phase first, then staged writes.
For the emulator backend, the expected REST shape is `beginTransaction` -> document reads with the transaction id -> `commit`. Keep that flow in mind when implementing transactional reads before the write phase.
