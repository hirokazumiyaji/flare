# Task 04: Query Support And Deterministic Results

## Status

Done

## Goal

Make query behavior complete enough for the documented API and deterministic enough for tests, demos, and scripts.

## Why This Task Exists

Current query support only works in-memory, only for part of the API, and produces unstable `limit()` behavior because iteration order is not fixed.

## Deliverables

- Emulator-backed collection queries.
- Emulator-backed collection group queries.
- Deterministic query ordering by document path lexicographic order before `limit`.
- Runtime support for a smoother query builder surface.
- A documented ordering rule based on the flare relative document path string, not bare document ids.

## Behavior To Support

- `collection("users").where("age", ">=", 20).limit(10).get()`
- `collection_group("members").where("active", "==", true).get()`
- `collection("users").doc("1")`
- `collection("users").get()` for all direct children
- `collection("users").limit(10).get()` without forcing a prior `where`

## Work Items

- Extend method dispatch so `CollectionRef` and `CollectionGroupRef` can produce empty `QueryRef` objects.
- Support `.get()` and `.limit()` directly from collection references.
- Implement emulator query requests with the Firestore REST query endpoint.
- Keep supported operators aligned with the current runtime surface.
- Sort results by document path lexicographic order before applying `limit`.
- Fix the in-memory backend to use the same deterministic ordering rule.
- Add repeatability tests that run the same `limit()` query multiple times.

## Acceptance Criteria

- In-memory and emulator query results use the same visible shape.
- Repeating the same `limit()` query returns the same documents in the same order.
- Collection group queries work against the emulator.
- The query builder no longer requires a `where` call before `limit` or `get`.
- The ordering rule is documented and shared with Task 01 and Task 06.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).
- Depends on [Task 02](02-cli-backend-selection.md).
- Depends on [Task 03](03-emulator-crud-and-codec.md).

## Notes

Use one ordering rule everywhere. For MVP that rule is lexicographic order on the flare relative document path string, for example `users/a < users/b` and `teams/t1/members/a < teams/t2/members/a`.
