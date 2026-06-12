# Task 03: Emulator CRUD, Merge, Sentinels, And Value Codec

## Status

Done

## Goal

Implement the emulator backend so it can round-trip the documented Firestore value types and CRUD operations.

Here, "merge" means Firestore field merge semantics for `set(..., merge: true)`.

## Why This Task Exists

The current emulator adapter is only a smoke-test stub. It does not implement query support, ignores merge semantics, decodes reads incorrectly, and cannot represent the full documented value surface.

## Deliverables

- A robust encoder from `Value` to Firestore REST payloads.
- A robust decoder from Firestore REST payloads to `Value`.
- Correct `get`, `set`, `update`, and `delete` behavior.
- Correct merge semantics for `set(..., merge: true)`.
- Correct sentinel handling for `server_timestamp()`.

## Value Coverage Required For MVP

- `Int`
- `Str`
- `Bool`
- `Nil`
- `Array`
- `Map`
- `Timestamp`
- `ServerTimestamp` on writes
- `Float` is out of scope for MVP and should stay unsupported until the runtime adds it explicitly.

## Work Items

- Replace ad-hoc string slicing with structured JSON handling.
- Add Firestore document field encoding for nested maps and arrays.
- Add Firestore document field decoding for nested maps and arrays.
- Implement timestamp encoding and decoding in a stable project format.
- Implement `set(..., merge: true)` with the correct REST request shape.
- Keep `update` behavior strict when the document is missing.
- Return complete document maps from `get`, not a partially decoded placeholder.
- Ensure emulator-backed `delete` semantics match the README contract.

## Acceptance Criteria

- A document written through the emulator backend can be read back with the same logical `Value`.
- `set(..., merge: true)` only updates the requested fields.
- `server_timestamp()` writes resolve to a timestamp value readable from a later `get`.
- Nested arrays and maps survive round-trip tests.
- The emulator backend no longer relies on fragile string extraction.
- Unsupported numeric types fail clearly rather than degrading silently.

## Dependencies

- Depends on [Task 01](01-mvp-contract.md).
- Depends on [Task 02](02-cli-backend-selection.md).

## Notes

Using a small JSON crate is acceptable here. Manual JSON parsing is already the weakest part of the current implementation and will slow down every later task if it stays.
