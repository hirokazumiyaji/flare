# Flare MVP Task List

> **ARCHIVED (2026-06-12):** This is a historical planning document; all tasks are complete. Some details reflect the state at the time and may differ from the current implementation (for example, the emulator uses the gRPC SDK rather than REST). See [docs/spec.md](../../spec.md) for the current authoritative specification.

## Goal

Deliver a usable MVP of `flare` as a Firestore scripting tool, not just a language runtime.

This file is the planning index for MVP work. The authoritative MVP contract will be added to the README by Task 01.

The MVP is done when all of the following are true:

- `fl` can run against the Firestore emulator from the CLI.
- The README API surface matches actual behavior.
- `doc`, `collection`, `collection_group`, `get`, `set`, `update`, `delete`, query chaining, `batch`, and `transaction` work end-to-end against the emulator.
- Query results are deterministic.
- `batch` and `transaction` do not leave partial writes on failure.
- Emulator-backed tests cover the supported API surface.
- The release binary can be built from CI without undocumented prerequisites.

## Principles

- Keep the MVP small and explicit.
- Prefer a narrower contract over partial semantics.
- Avoid backend-specific behavior leaking into the language surface.
- Make runtime behavior deterministic before adding new features.

## Scope

In scope for MVP:

- In-memory backend for local unit tests.
- Firestore emulator backend for CLI and integration tests.
- Single-attempt transaction semantics with atomic commit and rollback on failure.
- Deterministic query behavior.
- Document reads and writes, collection queries, collection group queries, timestamps, arrays, maps, and sentinels already described in the README.
- Integer-only numeric semantics.

Out of scope for MVP:

- Authenticated production Firestore access.
- Automatic transaction retries.
- Float support.
- Pagination features beyond documented `limit`, such as cursors and offsets.
- `order_by`, listeners, and index-dependent features not already documented.
- mruby compatibility beyond the documented subset.

## Critical Path

1. [Task 01: Freeze the MVP contract](tasks/01-mvp-contract.md)
2. [Task 02: Wire the CLI to selectable backends](tasks/02-cli-backend-selection.md)
3. [Task 03: Finish emulator CRUD and value encoding](tasks/03-emulator-crud-and-codec.md)
4. [Task 04: Finish query support and make results deterministic](tasks/04-query-and-determinism.md)
5. [Task 05: Implement atomic `batch` and `transaction`](tasks/05-batch-and-transaction.md)
6. [Task 06: Build the emulator test matrix and CI coverage](tasks/06-test-matrix-and-ci.md)
7. [Task 07: Tighten docs and release readiness](tasks/07-docs-and-release-readiness.md)

## Task Checklist

- [x] [Task 01: MVP contract and semantics freeze](tasks/01-mvp-contract.md)
- [x] [Task 02: CLI backend selection and emulator wiring](tasks/02-cli-backend-selection.md)
- [x] [Task 03: Emulator CRUD, merge, sentinels, and value codec](tasks/03-emulator-crud-and-codec.md)
- [x] [Task 04: Query support, collection group support, and deterministic ordering](tasks/04-query-and-determinism.md)
- [x] [Task 05: Atomic `batch` and `transaction` semantics](tasks/05-batch-and-transaction.md)
- [x] [Task 06: Unit tests, emulator integration tests, and CI execution path](tasks/06-test-matrix-and-ci.md)
- [x] [Task 07: README, examples, and release checklist](tasks/07-docs-and-release-readiness.md)

## Dependency Map

- Task 01 blocks every other task.
- Task 02 depends on Task 01.
- Task 03 depends on Tasks 01 and 02.
- Task 04 depends on Tasks 01 through 03.
- Task 05 depends on Tasks 01 through 04.
- Task 06 depends on Tasks 01 through 05.
- Task 07 depends on Tasks 01 through 06.

## Suggested Execution Order

Phase 1:

- Task 01
- Task 02
- Task 03

Phase 2:

- Task 04
- Task 05

Phase 3:

- Task 06
- Task 07

## Definition Of Done

- `cargo test --locked` passes locally.
- Emulator-backed integration tests pass with documented environment variables.
- Repeated query executions return the same results in the same order.
- A failing `batch` or `transaction` leaves Firestore unchanged.
- `fl --backend emulator <script>` succeeds against the documented local emulator setup.
- CLI usage output documents backend selection and required emulator environment variables.
- `cargo build --locked --release --bin fl` produces the documented binary.
- README setup instructions are sufficient to run one emulator-backed script from a clean checkout.
