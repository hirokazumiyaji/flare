# Task 07: Docs And Release Readiness

## Status

Done

## Goal

Align the repository narrative with the implemented MVP and make the release path honest and repeatable.

## Why This Task Exists

The project is close to looking complete from the outside, so any mismatch between README claims and actual behavior will create immediate confusion.

## Deliverables

- Updated README with real setup instructions.
- Updated example scripts that reflect supported behavior.
- A short release checklist for MVP shipping.

## Work Items

- Rewrite the README usage section around actual backend selection.
- Add an emulator quickstart with exact environment variables.
- Document one primary local setup path for MVP: macOS/Linux shell with the Firestore emulator installed and reachable through the documented environment variables.
- Document the supported `batch` and `transaction` semantics.
- Document MVP exclusions so production Firestore support is not implied.
- Review example scripts so they cover the intended happy path.
- Add one example that exercises the emulator backend flow.
- Add a release checklist covering tests, binary build, and manual emulator smoke run.

## Acceptance Criteria

- A new contributor can run one emulator-backed script by following the README.
- No README section implies production Firestore support.
- `batch` and `transaction` behavior is documented in the same terms used by the tests.
- Example scripts align with the supported MVP contract.
- The README documents one primary local environment instead of several partial setup options.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).
- Depends on [Task 02](02-cli-backend-selection.md).
- Depends on [Task 03](03-emulator-crud-and-codec.md).
- Depends on [Task 04](04-query-and-determinism.md).
- Depends on [Task 05](05-batch-and-transaction.md).
- Depends on [Task 06](06-test-matrix-and-ci.md).

## Notes

This is the final tightening pass. Do not update the README early in ways that hide unfinished behavior.
Task 01 should add only the formal contract block. Task 07 is where the rest of the README is aligned with the finished implementation.
