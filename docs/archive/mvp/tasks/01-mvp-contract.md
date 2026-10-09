# Task 01: MVP Contract And Semantics Freeze

## Status

Done

## Goal

Freeze the exact behavior that counts as MVP so implementation work does not chase a moving target.

## Why This Task Exists

The current repository mixes three different stories:

- a language runtime that works in-memory,
- an emulator smoke test,
- a README that reads like a full Firestore tool.

This task collapses those into one contract.

## Deliverables

- A formal `MVP Contract` section in the README.
- A short summary link from the planning docs back to that README section.
- A backend support matrix.
- Explicit semantics for `batch` and `transaction`.
- Explicit exclusions for post-MVP features.

Task 01 updates only the contract block in the README. Broader README cleanup stays in Task 07.

## Decisions To Lock

- The CLI supports `in-memory` and `emulator` backends for MVP.
- The CLI keeps manual argument parsing for MVP. No CLI crate is added.
- Production Firestore access is post-MVP.
- Numeric support is `Int` only for MVP.
- Float support is explicitly out of scope until the runtime grows a `Float` value.
- `batch` supports write operations only.
- `update` on a missing document fails and never creates the document.
- `transaction` supports `get`, `set`, `update`, and `delete` for MVP.
- Multiple `get` calls are allowed before the first staged write, including repeated reads of the same document and reads of different documents.
- `transaction` reads must happen before the first staged write.
- `get` after the first staged write inside `transaction` fails with a clear error.
- Queries inside `transaction` are out of scope for MVP.
- `transaction` is single-attempt. No automatic retry logic is required for MVP.
- Runtime errors and backend errors inside `batch` or `transaction` must both result in zero persisted writes.
- Query ordering is deterministic by the flare relative document path string in lexicographic order, not by bare document id. For example, `users/a` sorts before `users/b`, and `teams/t1/members/a` sorts before `teams/t2/members/a`.

## Work Items

- Write a short MVP contract section that is stronger than the current README wording.
- Mark the README contract as the authoritative source and keep `docs/mvp/` as planning material.
- Define the backend matrix for `run_script`, `run_script_with_firestore`, and `fl`.
- Define the manual CLI usage shape and error/help behavior.
- Define the allowed operations inside `batch`.
- Define the allowed operations inside `transaction`.
- Define the missing-document behavior of `update`.
- Define the allowed `get` patterns before the first staged write in `transaction`.
- Define the read-before-write restriction inside `transaction`.
- Define what happens on runtime error inside `batch` and `transaction`.
- Define deterministic query ordering as document path lexicographic order.
- Define the numeric type contract for MVP.
- Mark unsupported features as explicitly out of scope.

## Acceptance Criteria

- Every later task can point back to this contract without reopening scope.
- README claims do not exceed implemented behavior.
- `batch` and `transaction` have unambiguous MVP semantics.
- `transaction` read behavior is defined without requiring a local overlay design.
- There is no unresolved ambiguity around backend behavior.
- CLI parsing policy is fixed before implementation starts.
- `limit` is clearly in scope while cursor and offset pagination are clearly out of scope.

## Dependencies

None.

## Notes

The key simplification is to make MVP mean "works against the emulator from the CLI" rather than "works against production Firestore."
