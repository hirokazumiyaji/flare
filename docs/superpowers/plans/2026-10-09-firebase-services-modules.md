# Firebase Services and Built-in Modules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete Issue #78 by grouping Firestore, Auth, and Storage in a composable Services API and exposing service-owned built-in modules without breaking existing embedding or script APIs.

**Architecture:** Keep the public `Runtime<F, S>` type parameters for source compatibility, but move evaluation into a non-generic evaluator borrowing a `ServiceAccess` view and persistent `RuntimeState`. Represent unconfigured services explicitly, and route both module calls and existing global function names through shared built-in call definitions. Resolve CLI project settings once and pass timeout configuration to each selected backend.

**Tech Stack:** Rust, mruby-free custom lexer/parser/evaluator, Firebase emulator REST clients, Cargo integration tests.

**Spec:** `docs/design/firebase-services-and-modules.md`

## Global Constraints

- Keep `Runtime<F, S>`'s existing generic parameters and existing constructors source-compatible.
- Keep Runtime and service traits synchronous; do not add `Send` / `Sync` bounds or an async worker.
- Keep Auth sessions owned by the Auth backend; never expose passwords or tokens through Values, display, or errors.
- Do not forward Auth tokens to Firestore or Storage; do not add cross-service transaction semantics.
- Preserve borrowed Storage support and do not add a `'static` bound to `Storage`.
- The new `Services` API must not silently choose an emulator or fall back to in-memory services.
- Existing global script functions remain aliases of shared handlers and retain their contracts.
- Use `ServiceId::{Firestore, Auth, Storage}` and capability errors with source spans for unavailable services.
- Keep the current `flare-test` CLI project default; reject conflicting nonempty project environment variables.
- Default Auth emulator options to `fake-api-key` and a 5,000 ms operation timeout; explicit construction must not read environment variables.
- One absolute deadline bounds each built-in network operation; synchronous DNS means Storage timeout support remains false until full-operation enforcement exists.

## Review Focus

- An unconfigured service call must return a typed capability error before evaluating side-effecting arguments.
- An Auth backend configured with no signed-in user must return `nil`, distinct from unavailable Auth.
- Invalid receiver calls must evaluate the receiver once, then validate the call contract before argument side effects.
- Auth and Storage timeouts must cover slow incremental responses under one operation deadline without exposing credentials.
- Repeated `Runtime::run` calls must preserve backend-owned Auth sessions and failed transaction rollback state.

---

### Task 1: Add Services and consolidate existing entry points

**Files:**
- Create: `src/services.rs`
- Modify: `src/lib.rs`, `src/runtime.rs`
- Test: `tests/services.rs`

**Interfaces:**
- Produces `ProjectConfig { project_id: Option<String>, storage_bucket: Option<String> }`.
- Produces `Services<F: Firestore, S: Storage = InMemoryStorage>` with `in_memory`, `with_firestore`, `with_storage`, `with_auth`, and `with_project`.
- Produces `Runtime::from_services`, `Runtime::from_services_with_limits`, `run_script_with_services`, and `run_script_with_services_and_limits`.

- [ ] Write tests that combine distinct recording Firestore, Auth, and Storage backends in one script; verify `Runtime<InMemoryFirestore>` type annotations and `Services::in_memory().with_storage(&mut storage)` compile.
- [ ] Run the focused services test and confirm the missing Services APIs fail compilation.
- [ ] Add Services and ProjectConfig; group Runtime's existing service ownership in Services.
- [ ] Add the new constructors and entry points, then delegate existing constructors and `run_script_with_*` functions through Services without changing their signatures or default in-memory behavior.
- [ ] Run `cargo test --locked --test services --test auth --test storage --test in_memory`.

### Task 2: Separate persistent runtime state from the evaluator

**Files:**
- Modify: `src/runtime.rs`
- Create: `src/runtime/state.rs`, `src/runtime/evaluator.rs`, `src/runtime/builtins/mod.rs`, `src/runtime/builtins/core.rs`, `src/runtime/builtins/firestore.rs`, `src/runtime/builtins/auth.rs`, `src/runtime/builtins/storage.rs`
- Test: existing runtime, budget, transaction, and in-memory tests

**Interfaces:**
- Produces non-generic `ServiceAccess<'a>` borrowing `&'a mut dyn Firestore`, `&'a mut dyn Storage`, `&'a mut dyn Auth`, and `&'a ProjectConfig`.
- Produces non-generic `RuntimeState` for scopes, execution context, rollback state, limits, usage, call depth, and execution lifecycle.
- Produces non-generic `Evaluator<'a>` over `ServiceAccess<'a>` and mutable `RuntimeState`.

- [ ] Preserve current test coverage for scope behavior, panic recovery, return propagation, budget counters, query shaping, codec depth, and transaction rollback retry; verify Auth sessions and pending rollback state survive repeated `Runtime::run` calls.
- [ ] Move evaluator methods, ScopeGuard, and built-in handlers behind the non-generic Evaluator; keep persistent state in RuntimeState across `run` calls.
- [ ] Make Runtime borrow Services and RuntimeState to construct an Evaluator for each evaluation.
- [ ] Run `cargo test --locked`.

### Task 3: Represent unconfigured services and capability errors

**Files:**
- Create: `src/firestore/unavailable.rs`, `src/auth/unavailable.rs`, `src/storage/unavailable.rs`
- Modify: `src/services.rs`, `src/firestore/mod.rs`, `src/auth/mod.rs`, `src/storage/mod.rs`, `src/error.rs`, `src/runtime/evaluator.rs`
- Test: `tests/services.rs`, `tests/in_memory.rs`

**Interfaces:**
- Produces `ServiceId::{Firestore, Auth, Storage}` and downcastable `CapabilityError { service: ServiceId, span: Option<Span> }`.
- Adds default `is_available(&self) -> bool` to each service trait; unavailable implementations override it and return service-specific trait errors.
- Adds `Services::empty(project: ProjectConfig)` using `UnavailableFirestore`, `UnavailableAuth`, and `UnavailableStorage`.

- [ ] Test service-specific capability errors, downcasting and spans, configured-but-signed-out Auth returning Nil, and no argument side effects when Auth, Storage, Firestore operations, or query `get/count` are unavailable.
- [ ] Implement unavailable backends and defaulted availability methods, forwarding availability through existing boxed and borrowed trait implementations.
- [ ] Check required service availability before evaluating call arguments while preserving Firestore reference/query/sentinel construction without I/O.
- [ ] Run `cargo test --locked --test services --test in_memory`.

### Task 4: Add built-in module Values and shared call definitions

**Files:**
- Modify: `src/value.rs`, `src/runtime/evaluator.rs`, `src/runtime/builtins/*`, `src/firestore/mod.rs`
- Create: `tests/modules.rs`

**Interfaces:**
- Produces `BuiltinModule::{Firestore, Auth, Storage}` and `Value::Module(BuiltinModule)`.
- Produces a non-generic `BuiltinSpec` containing owner, module method, optional global name, call contract, required service, and handler.

- [ ] Test module aliases, local shadowing, module display/equality/truthiness, module calls inside functions, nested module rejection in stored/query/transform values, and unknown module methods.
- [ ] Test shared behavior between module methods and existing globals, including positional/keyword duplicates, invalid blocks, receiver evaluation once, source-order arguments, and transaction semantics.
- [ ] Resolve ordinary variables and functions before built-in module names; make module Values immutable leaves for depth validation.
- [ ] Consolidate built-in name lists, handler lookup, CallContracts, and service ownership in BuiltinSpec; dispatch module/global calls to the same handler.
- [ ] Apply the specified call order and support module keywords/blocks, including `Firestore.set(..., merge: true)`, `Storage.upload(..., content_type: ...)`, `Firestore.batch { ... }`, and `Firestore.transaction { ... }`.
- [ ] Run `cargo test --locked --test modules --test services --test auth --test storage --test in_memory`.

### Task 5: Construct CLI services from one project configuration

**Files:**
- Modify: `src/main.rs`, `src/auth/emulator.rs`, explicit Firestore and Storage construction paths
- Test: `tests/cli.rs`, `tests/firebase_config.rs`

**Interfaces:**
- Produces `AuthEmulatorOptions { api_key: String, operation_timeout: Option<Duration> }` with defaults `fake-api-key` and 5,000 ms, and `AuthEmulator::new_with_options(host, project_id, options)` that does not read environment variables.

- [ ] Test each supported project variable, identical values, conflicting values, unset `flare-test` default, explicit bucket precedence, and explicit Auth emulator options without environment reads.
- [ ] Test explicit emulator selection with missing host/feature and preserve current Storage `auto` selection behavior.
- [ ] Resolve project settings once in the CLI, create explicit service backends with the shared project, and call the new services entry point once.
- [ ] Keep existing AuthEmulator `new` and `from_env` signatures by resolving environment options then delegating to `new_with_options`.
- [ ] Add required Firestore boxed-backend forwarding for query, transaction, timeout, and depth hooks.
- [ ] Run `cargo test --locked --test cli --test firebase_config`.

### Task 6: Propagate backend timeouts and document synchronous limits

**Files:**
- Modify: service traits and implementations, `src/auth/emulator.rs`, `src/storage/emulator.rs`, `src/firestore/emulator/client.rs`, `src/runtime.rs`, `tests/budgets.rs`
- Test: service backend unit tests and local delayed-server tests

**Interfaces:**
- Add default no-op `set_timeout(Option<Duration>)` and `supports_backend_timeout() -> bool` hooks to Auth and Storage; forward them through boxed and borrowed implementations.
- Add `Runtime::service_backend_timeout_supported(service: ServiceId) -> bool`; keep `backend_timeout_supported()` reporting Firestore support.

- [ ] Test timeout propagation and support reporting for custom, boxed, borrowed, and built-in backends.
- [ ] Test stalled and incrementally slow response bodies against one absolute per-operation deadline without external network access.
- [ ] Apply Auth timeout to send plus body read; apply Storage's remaining deadline to connect/write/read and retries. Keep Storage support false while synchronous DNS is not covered.
- [ ] Test nested Tokio calls return explanatory Backend/Auth Network errors without panic, while in-memory execution remains available.
- [ ] Test timeout errors redact tokens and preserve transaction cleanup and Runtime reuse.
- [ ] Run `cargo test --locked --test budgets` and `cargo test --locked --features emulator`.

### Task 7: Share service contracts across in-memory and emulator backends

**Files:**
- Modify: `tests/common/*`, `tests/modules.rs`, `tests/services.rs`, `tests/auth_emulator.rs`, `tests/storage_emulator.rs`, `tests/firestore_emulator.rs`

- [ ] Run shared language scripts against in-memory and emulator backends and compare return shapes, display, and operation contracts without requiring identical tokens or emulator UIDs.
- [ ] Verify a single script can use all three services and mixed backend selections reach the selected implementations.
- [ ] Preserve deny-rules coverage and ensure configured emulator suites fail rather than silently skip configured services.
- [ ] Run `cargo test --locked` and, with all three local emulators running, `cargo test --locked --features emulator`.

### Task 8: Align specification, examples, and user documentation

**Files:**
- Modify: `docs/spec.md`, `README.md`, `docs/firebase-security.md`, `examples/scripts/*`, `tests/example_scripts.rs`

- [ ] Document Services and module APIs, unconfigured-service behavior, timeout support, synchronous execution limits, project resolution, Auth identity boundaries, and transaction scope.
- [ ] Add an all-services module example and retain an example covering existing global APIs.
- [ ] Correct stale statements about Auth/Storage availability and StorageEmulator Cargo feature availability.
- [ ] Run `cargo test --locked --doc --test example_scripts`.

## Dependencies and acceptance

Recommended order: Task 1 → 2 → 3 → 4 → 5 and 6 → 7 → 8. Update `docs/spec.md` with each behavior change, then perform final integration in Task 8.

Issue #78 is complete when existing Firestore-only callers remain compatible, Auth and Storage can be selected independently, scripts run with in-memory services without emulators, built-ins do not gain service generic parameters, unavailable capabilities are explicit, module and legacy APIs share behavior, and timeout/async constraints match the implementation and documentation.
