use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::time::{Duration, Instant};

use crate::ast::{BinOp, Block, CallArg, Expr, Stmt, StrPiece, UnOp};
use crate::auth::{Auth, AuthError};
use crate::error::{CapabilityError, FlareError, ServiceId, Span};
use crate::firestore::{
    normalize_query_field_path, validate_firestore_id, validate_write_data, Firestore,
    QueryCandidates, QueryResultLimitExceeded, WriteOp,
};
use crate::limits::{RuntimeLimits, RuntimeUsage};
use crate::parser::Parser;
use crate::services::ServiceAccess;
use crate::services::Services;
use crate::storage::{validate_content_type, validate_storage_path, InMemoryStorage, Storage};
use crate::value::{
    self, BuiltinModule, CollectionGroupRef, CollectionRef, Direction, DocRef, QueryFilter,
    QueryOp, QueryRef, Timestamp, UserFunc, Value,
};

mod evaluator;
mod state;

use evaluator::{auth_session_field, Evaluator};
#[cfg(test)]
use evaluator::{builtin_specs, is_builtin_name, ScopeGuard};
use state::RuntimeState;

enum ExecCtx {
    Normal,
    Batch { writes: Vec<WriteOp> },
    Transaction { writes: Vec<WriteOp> },
}

/// Propagate the parts of the budget that constrain the *backend*.
///
/// A backend that recurses while converting values (the emulator codec) must
/// know the depth ceiling, otherwise it overflows the native stack before the
/// runtime can validate the result. A disabled value-depth budget still needs a
/// finite number for that recursion guard, so the shipped default is used as
/// the backstop.
fn apply_backend_limits<F: Firestore>(fs: &mut F, limits: &RuntimeLimits) {
    fs.set_timeout(limits.backend_timeout);
    fs.set_max_value_depth(
        limits
            .max_value_depth
            .unwrap_or(crate::limits::DEFAULT_MAX_VALUE_DEPTH),
    );
}

enum MethodReceiver {
    Collection(CollectionRef),
    CollectionGroup(CollectionGroupRef),
    Query(QueryRef),
    Array(Vec<Value>),
    Timestamp(Timestamp),
    Bytes(Vec<u8>),
    AuthSession(crate::auth::AuthSession),
    Map(BTreeMap<String, Value>),
    Module(BuiltinModule),
    Str(String),
    Int(i64),
    Other(Value),
}

impl MethodReceiver {
    pub fn into_value(self) -> Value {
        match self {
            Self::Collection(c) => Value::CollectionRef(c),
            Self::CollectionGroup(cg) => Value::CollectionGroupRef(cg),
            Self::Query(q) => Value::QueryRef(q),
            Self::Array(a) => Value::Array(a),
            Self::Timestamp(ts) => Value::Timestamp(ts),
            Self::Bytes(b) => Value::Bytes(b),
            Self::AuthSession(s) => Value::AuthSession(s),
            Self::Map(m) => Value::Map(m),
            Self::Module(m) => Value::Module(m),
            Self::Str(s) => Value::Str(s),
            Self::Int(i) => Value::Int(i),
            Self::Other(v) => v,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Collection(_) => "CollectionRef",
            Self::CollectionGroup(_) => "CollectionGroupRef",
            Self::Query(_) => "QueryRef",
            Self::Array(_) => "Array",
            Self::Timestamp(_) => "Timestamp",
            Self::Bytes(_) => "Bytes",
            Self::AuthSession(_) => "AuthSession",
            Self::Map(_) => "Map",
            Self::Module(_) => "Module",
            Self::Str(_) => "Str",
            Self::Int(_) => "Int",
            Self::Other(v) => v.type_name(),
        }
    }
}

pub struct Runtime<F: Firestore, S: Storage = InMemoryStorage> {
    services: Services<F, S>,
    state: RuntimeState,
}

impl<F: Firestore> Runtime<F, InMemoryStorage> {
    /// Construct a runtime with conservative, finite limits.
    pub fn new(fs: F) -> Self {
        Self::with_limits(fs, RuntimeLimits::default())
    }

    /// Construct a runtime with embedder-provided limits.
    ///
    /// The backend hook applies to subsequent operations. A
    /// `FirestoreEmulator` that must also apply the same deadline while
    /// connecting should be created with `FirestoreEmulator::new_with_timeout`.
    pub fn with_limits(fs: F, limits: RuntimeLimits) -> Self {
        Self::with_storage_and_limits(fs, InMemoryStorage::default(), limits)
    }

    /// Explicit constructor for trusted scripts with all budgets disabled.
    pub fn new_unlimited(fs: F) -> Self {
        Self::with_limits(fs, RuntimeLimits::unlimited())
    }

    /// Construct a runtime with an explicit authentication backend.
    ///
    /// Defined on the default-storage specialization so the `S` parameter
    /// never needs an annotation: `Runtime::new_with_auth(fs, auth)` infers
    /// `S = InMemoryStorage`. Hosts that need a different Storage backend
    /// can use [`Self::with_storage_and_auth`].
    pub fn new_with_auth<A: Auth + 'static>(fs: F, auth: A) -> Self {
        Self::with_storage_and_auth(fs, InMemoryStorage::default(), auth)
    }

    /// Alias for [`Runtime::new_with_auth`] that reads naturally at call sites.
    pub fn with_auth<A: Auth + 'static>(fs: F, auth: A) -> Self {
        Self::new_with_auth(fs, auth)
    }
}

impl<F: Firestore, S: Storage> Runtime<F, S> {
    /// Construct a runtime with an independent Storage backend. Firestore and
    /// Storage deliberately remain separate services; the default runtime
    /// uses [`InMemoryStorage`].
    pub fn with_storage(fs: F, storage: S) -> Self {
        Self::with_storage_and_limits(fs, storage, RuntimeLimits::default())
    }

    /// Construct a runtime with explicit Storage and budgets.
    pub fn with_storage_and_limits(fs: F, storage: S, limits: RuntimeLimits) -> Self {
        Self::from_services_with_limits(
            Services::in_memory()
                .with_firestore(fs)
                .with_storage(storage),
            limits,
        )
    }

    pub fn from_services(services: Services<F, S>) -> Self {
        Self::from_services_with_limits(services, RuntimeLimits::default())
    }

    pub fn from_services_with_limits(mut services: Services<F, S>, limits: RuntimeLimits) -> Self {
        apply_backend_limits(&mut services.firestore, &limits);
        services.auth.set_timeout(limits.backend_timeout);
        services.storage.set_timeout(limits.backend_timeout);
        Self {
            services,
            state: RuntimeState::new(limits),
        }
    }

    /// Construct a runtime with explicit Storage and Auth backends.
    pub fn with_storage_and_auth<A: Auth + 'static>(fs: F, storage: S, auth: A) -> Self {
        Self::from_services(
            Services::in_memory()
                .with_firestore(fs)
                .with_storage(storage)
                .with_auth(auth),
        )
    }

    /// Construct a runtime with explicit Storage, Auth, and budgets.
    pub fn with_storage_limits_and_auth<A: Auth + 'static>(
        fs: F,
        storage: S,
        auth: A,
        limits: RuntimeLimits,
    ) -> Self {
        Self::from_services_with_limits(
            Services::in_memory()
                .with_firestore(fs)
                .with_storage(storage)
                .with_auth(auth),
            limits,
        )
    }

    pub fn limits(&self) -> RuntimeLimits {
        self.state.limits
    }

    pub fn project_config(&self) -> &crate::services::ProjectConfig {
        &self.services.project
    }

    /// Report whether the configured backend can enforce `backend_timeout`.
    /// Legacy synchronous backends may retain the trait's compatibility
    /// no-op; the built-in emulator returns `true`.
    pub fn backend_timeout_supported(&self) -> bool {
        self.services.firestore.supports_backend_timeout()
    }

    pub fn service_backend_timeout_supported(&self, service: ServiceId) -> bool {
        match service {
            ServiceId::Firestore => self.services.firestore.supports_backend_timeout(),
            ServiceId::Auth => self.services.auth.supports_backend_timeout(),
            ServiceId::Storage => self.services.storage.supports_backend_timeout(),
            ServiceId::IO => false,
        }
    }

    pub fn set_limits(&mut self, limits: RuntimeLimits) {
        self.state.limits = limits;
        apply_backend_limits(&mut self.services.firestore, &self.state.limits);
        self.services.auth.set_timeout(limits.backend_timeout);
        self.services.storage.set_timeout(limits.backend_timeout);
    }

    /// Return a snapshot of the counters consumed by the most recent run.
    pub fn usage(&self) -> RuntimeUsage {
        let mut usage = self.state.usage;
        usage.elapsed = self
            .state
            .started_at
            .map(|start| start.elapsed())
            .unwrap_or_default();
        usage
    }

    pub fn reset_usage(&mut self) {
        self.state.usage = RuntimeUsage::default();
        self.state.call_depth = 0;
        self.state.started_at = None;
        self.state.execution_active = false;
    }

    pub fn auth(&self) -> &dyn Auth {
        self.services.auth.as_ref()
    }

    pub fn auth_mut(&mut self) -> &mut dyn Auth {
        self.services.auth.as_mut()
    }

    pub fn eval_program(&mut self, stmts: &[Stmt]) -> Result<Value, Box<dyn Error>> {
        self.evaluator().eval_program(stmts)
    }

    pub fn eval_program_with_spans(
        &mut self,
        stmts: &[Stmt],
        spans: &[Span],
    ) -> Result<Value, Box<dyn Error>> {
        self.evaluator().eval_program_with_spans(stmts, spans)
    }

    pub fn set_repl_mode(&mut self, enabled: bool) {
        self.state.repl_mode = enabled;
    }

    pub fn repl_mode(&self) -> bool {
        self.state.repl_mode
    }

    pub fn global_variable_names(&self) -> Vec<String> {
        self.state
            .scopes
            .first()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn run(&mut self, source: &str) -> Result<Value, Box<dyn Error>> {
        self.evaluator().run(source)
    }

    fn evaluator(&mut self) -> Evaluator<'_> {
        let services = ServiceAccess {
            firestore: &mut self.services.firestore,
            storage: &mut self.services.storage,
            auth: self.services.auth.as_mut(),
            io: self.services.io.as_mut(),
            project: &self.services.project,
        };
        Evaluator::new(services, &mut self.state)
    }
}

/// Convert an evaluated receiver into the exhaustive set accepted by method
/// dispatch. Keeping this conversion next to the dispatch match prevents a
/// new receiver from being accepted by one path but rejected by the other.
fn into_method_receiver(value: Value) -> Result<MethodReceiver, Box<dyn Error>> {
    Ok(match value {
        Value::CollectionRef(value) => MethodReceiver::Collection(value),
        Value::CollectionGroupRef(value) => MethodReceiver::CollectionGroup(value),
        Value::QueryRef(value) => MethodReceiver::Query(value),
        Value::Array(value) => MethodReceiver::Array(value),
        Value::Timestamp(value) => MethodReceiver::Timestamp(value),
        Value::Bytes(value) => MethodReceiver::Bytes(value),
        Value::AuthSession(value) => MethodReceiver::AuthSession(value),
        Value::Map(value) => MethodReceiver::Map(value),
        Value::Module(value) => MethodReceiver::Module(value),
        Value::Str(value) => MethodReceiver::Str(value),
        Value::Int(value) => MethodReceiver::Int(value),
        other => MethodReceiver::Other(other),
    })
}

fn validate_unique_keywords(args: &[CallArg]) -> Result<(), Box<dyn Error>> {
    let mut seen = BTreeSet::<&str>::new();
    for arg in args {
        if let CallArg::Keyword { name, .. } = arg {
            if !seen.insert(name.as_str()) {
                return Err(format!("duplicate keyword argument: {name}").into());
            }
        }
    }
    Ok(())
}

fn positional_arg_count(args: &[CallArg]) -> usize {
    args.iter()
        .filter(|arg| matches!(arg, CallArg::Positional(_)))
        .count()
}

fn validate_unique_params(params: &[String], kind: &str) -> Result<(), Box<dyn Error>> {
    let mut seen = HashSet::with_capacity(params.len());
    for param in params {
        if !seen.insert(param.as_str()) {
            return Err(format!("duplicate {kind} parameter: {param}").into());
        }
    }
    Ok(())
}

fn keyword_names(args: &[CallArg]) -> BTreeMap<String, Value> {
    args.iter()
        .filter_map(|arg| match arg {
            CallArg::Keyword { name, .. } => Some((name.clone(), Value::Nil)),
            _ => None,
        })
        .collect()
}

fn validate_user_call_contract(
    name: &str,
    params: &[String],
    args: &[CallArg],
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if block.is_some() {
        return Err(format!("{name} does not take a block (block is not allowed)").into());
    }
    bind_function_args(
        params,
        vec![Value::Nil; positional_arg_count(args)],
        keyword_names(args),
    )
    .map(|_| ())
}

fn bind_function_args(
    params: &[String],
    positional: Vec<Value>,
    mut kwargs: BTreeMap<String, Value>,
) -> Result<Vec<(String, Value)>, Box<dyn Error>> {
    if positional.len() > params.len() {
        return Err("too many positional args".into());
    }
    let mut out = vec![];
    let mut idx = 0;
    for param in params {
        if idx < positional.len() {
            if kwargs.contains_key(param) {
                return Err(format!(
                    "duplicate argument {param}: specified both positionally and as a keyword"
                )
                .into());
            }
            out.push((param.clone(), positional[idx].clone()));
            idx += 1;
            continue;
        }
        if let Some(v) = kwargs.remove(param) {
            out.push((param.clone(), v));
            continue;
        }
        return Err(format!("missing argument: {param}").into());
    }
    if !kwargs.is_empty() {
        let keys: Vec<String> = kwargs.keys().cloned().collect();
        return Err(format!("unknown keyword arguments: {}", keys.join(", ")).into());
    }
    Ok(out)
}

/// `bytes(...)` の引数エラー文言。3 箇所から使うので定数にまとめる。
const BYTES_USAGE: &str = "bytes expects one string or an array of integers from 0 to 255";
const BYTES_ELEMENT_ERR: &str = "bytes array elements must be integers from 0 to 255";

/// The positional part of a call contract.
#[derive(Clone, Copy)]
enum PositionalArity {
    Exact(usize),
    Range(usize, usize),
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockPolicy {
    Forbidden,
    Required,
}

/// Runtime validation shared by builtin functions and built-in methods.
///
/// `keywords` is intentionally small: the language's builtin API is
/// positional except for `set(..., merge: true)`.  `array` and `puts` use
/// `PositionalArity::Any` because their variable-arity behavior is part of
/// the API.
#[derive(Clone, Copy)]
struct CallContract {
    positional: PositionalArity,
    keywords: &'static [&'static str],
    block: BlockPolicy,
    /// Keywords that occupy the same slot as a positional argument.
    positional_keyword_conflicts: &'static [(&'static str, usize)],
    /// Optional compatibility/detail hint for a missing argument.
    missing_hint: Option<&'static str>,
}

impl CallContract {
    const fn exact(positional: usize) -> Self {
        Self {
            positional: PositionalArity::Exact(positional),
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    const fn range(min: usize, max: usize) -> Self {
        Self {
            positional: PositionalArity::Range(min, max),
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    const fn any() -> Self {
        Self {
            positional: PositionalArity::Any,
            keywords: &[],
            block: BlockPolicy::Forbidden,
            positional_keyword_conflicts: &[],
            missing_hint: None,
        }
    }

    const fn with_keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }

    const fn with_positional_keyword_conflicts(
        mut self,
        conflicts: &'static [(&'static str, usize)],
    ) -> Self {
        self.positional_keyword_conflicts = conflicts;
        self
    }

    const fn with_missing_hint(mut self, hint: &'static str) -> Self {
        self.missing_hint = Some(hint);
        self
    }

    const fn requiring_block(mut self) -> Self {
        self.block = BlockPolicy::Required;
        self
    }
}

fn validate_call_contract(
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
    contract: CallContract,
) -> Result<(), Box<dyn Error>> {
    // Check this before arity/type checks so a call which supplies the same
    // option in two ways always gets the actionable duplicate-argument error.
    for (keyword, position) in contract.positional_keyword_conflicts {
        if positional_len > *position && kwargs.contains_key(*keyword) {
            return Err(format!(
                "duplicate argument {keyword}: specified both positionally and as a keyword"
            )
            .into());
        }
    }

    match contract.positional {
        PositionalArity::Any => {}
        PositionalArity::Exact(expected) => {
            if positional_len < expected {
                let hint = contract
                    .missing_hint
                    .map(|hint| format!(" ({hint})"))
                    .unwrap_or_default();
                return Err(format!(
                    "{name} is missing positional arguments: expected {expected}, got {positional_len}{hint}"
                )
                .into());
            }
            if positional_len > expected {
                return Err(too_many_positional_error(
                    name,
                    expected,
                    positional_len,
                    true,
                ));
            }
        }
        PositionalArity::Range(min, max) => {
            if positional_len < min {
                return Err(format!(
                    "{name} expects at least {min} positional arguments, got {positional_len}"
                )
                .into());
            }
            if positional_len > max {
                return Err(too_many_positional_error(name, max, positional_len, false));
            }
        }
    }

    let unknown: Vec<&str> = kwargs
        .keys()
        .filter(|key| {
            !contract
                .keywords
                .iter()
                .any(|allowed| allowed == &key.as_str())
        })
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!("unknown keyword arguments: {}", unknown.join(", ")).into());
    }

    match (contract.block, block) {
        (BlockPolicy::Forbidden, Some(_)) => {
            return Err(format!("{name} does not take a block (block is not allowed)").into());
        }
        (BlockPolicy::Required, None) => {
            return Err(format!("{name} requires {{ ... }} block").into());
        }
        _ => {}
    }

    Ok(())
}

fn too_many_positional_error(
    name: &str,
    expected: usize,
    actual: usize,
    exact: bool,
) -> Box<dyn Error> {
    if expected == 0 {
        // Keep the long-standing array-method wording while making the
        // contract failure explicit for new callers and diagnostics.
        format!(
            "{name} does not take arguments: too many positional arguments (expected 0, got {actual})"
        )
        .into()
    } else {
        let expectation = if exact { "exactly" } else { "at most" };
        format!(
            "too many positional arguments for {name} (expected {expectation} {expected}, got {actual})"
        )
        .into()
    }
}

fn unknown_keyword_arguments(kwargs: &BTreeMap<String, Value>) -> Box<dyn Error> {
    let keys: Vec<&str> = kwargs.keys().map(String::as_str).collect();
    format!("unknown keyword arguments: {}", keys.join(", ")).into()
}

fn query_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "where" => Some(CallContract::exact(3)),
        "order_by" => Some(CallContract::exact(2)),
        "limit" | "offset" => Some(CallContract::exact(1)),
        "count" | "get" => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn auth_session_method_contract(name: &str) -> Option<CallContract> {
    // Field-style access has no fixed method set; any name with no args is a
    // field read, anything else is rejected by dispatch.
    let _ = name;
    Some(CallContract::exact(0))
}

fn bytes_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "len" => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn array_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "each" | "map" | "filter" | "select" | "reject" | "find" | "each_with_index"
        | "map_with_index" => Some(CallContract::exact(0).requiring_block()),
        "reduce" | "inject" => Some(CallContract::range(0, 1).requiring_block()),
        "len" => Some(CallContract::exact(0)),
        "contains" => Some(CallContract::exact(1)),
        "join" => Some(CallContract::range(0, 1)),
        _ => None,
    }
}

fn int_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "times" => Some(CallContract::exact(0).requiring_block()),
        "to_i" => Some(CallContract::exact(0)),
        _ => None,
    }
}

fn str_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "len" | "trim" | "to_i" => Some(CallContract::exact(0)),
        "split" | "starts_with" | "ends_with" | "contains" => Some(CallContract::exact(1)),
        _ => None,
    }
}

fn map_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "keys" | "values" | "len" => Some(CallContract::exact(0)),
        "has_key" => Some(CallContract::exact(1)),
        _ => Some(CallContract::exact(0)),
    }
}

fn timestamp_method_contract(name: &str) -> Option<CallContract> {
    match name {
        "to_unix" | "to_rfc3339" => Some(CallContract::exact(0)),
        "add_seconds" => Some(CallContract::exact(1)),
        _ => None,
    }
}

fn method_contract(receiver: &MethodReceiver, name: &str) -> Option<CallContract> {
    if matches!(name, "to_s" | "inspect" | "type_of") {
        return Some(CallContract::exact(0));
    }
    match receiver {
        MethodReceiver::Collection(_) if name == "doc" => Some(CallContract::exact(1)),
        MethodReceiver::Collection(_)
        | MethodReceiver::CollectionGroup(_)
        | MethodReceiver::Query(_) => query_method_contract(name),
        MethodReceiver::Array(_) => array_method_contract(name),
        MethodReceiver::Str(_) => str_method_contract(name),
        MethodReceiver::Timestamp(_) => timestamp_method_contract(name),
        MethodReceiver::Bytes(_) => bytes_method_contract(name),
        MethodReceiver::AuthSession(_) => auth_session_method_contract(name),
        MethodReceiver::Map(_) => map_method_contract(name),
        MethodReceiver::Int(_) => int_method_contract(name),
        MethodReceiver::Module(_) | MethodReceiver::Other(_) => None,
    }
}

fn method_call_label(receiver: &MethodReceiver, name: &str) -> String {
    match receiver {
        MethodReceiver::Collection(_) if name == "doc" => format!("collection method {name}"),
        MethodReceiver::Collection(_)
        | MethodReceiver::CollectionGroup(_)
        | MethodReceiver::Query(_) => format!("query method {name}"),
        MethodReceiver::Array(_) => format!("array method {name}"),
        MethodReceiver::Str(_) => format!("string method {name}"),
        MethodReceiver::Int(_) => format!("int method {name}"),
        MethodReceiver::Timestamp(_) => format!("timestamp method {name}"),
        MethodReceiver::Bytes(_) => format!("bytes method {name}"),
        MethodReceiver::AuthSession(_) => format!("auth session method {name}"),
        MethodReceiver::Map(_) => match name {
            "keys" | "values" | "len" | "has_key" => format!("map method {name}"),
            _ => format!("map key access {name}"),
        },
        MethodReceiver::Module(module) => format!("{}.{name}", module.name()),
        MethodReceiver::Other(_) => format!("{} method {name}", receiver.type_name()),
    }
}

fn validate_method_call(
    receiver: &MethodReceiver,
    name: &str,
    positional_len: usize,
    kwargs: &BTreeMap<String, Value>,
    block: Option<&Block>,
) -> Result<(), Box<dyn Error>> {
    if let Some(contract) = method_contract(receiver, name) {
        return validate_call_contract(
            &method_call_label(receiver, name),
            positional_len,
            kwargs,
            block,
            contract,
        );
    }

    // Even an unknown method must not silently accept keyword arguments or a
    // block.  The eventual dispatch error (unknown method / unsupported
    // receiver) is returned only after these contract-level mistakes.
    if !kwargs.is_empty() {
        return Err(unknown_keyword_arguments(kwargs));
    }
    if block.is_some() {
        return Err(format!(
            "{} does not take a block (block is not allowed)",
            method_call_label(receiver, name)
        )
        .into());
    }
    Err(match receiver {
        MethodReceiver::Collection(_)
        | MethodReceiver::CollectionGroup(_)
        | MethodReceiver::Query(_) => format!("unknown query method: {name}").into(),
        MethodReceiver::Array(_) => format!("unknown array method: {name}").into(),
        MethodReceiver::Str(_) => format!("unknown string method: {name}").into(),
        MethodReceiver::Int(_) => format!("unknown int method: {name}").into(),
        MethodReceiver::Timestamp(_) => format!("unknown timestamp method: {name}").into(),
        MethodReceiver::Bytes(_) => format!("unknown bytes method: {name}").into(),
        MethodReceiver::AuthSession(_) => format!("unknown auth session method: {name}").into(),
        MethodReceiver::Map(_) => format!("unknown map method: {name}").into(),
        MethodReceiver::Module(module) => {
            format!("unknown module method: {}.{name}", module.name()).into()
        }
        MethodReceiver::Other(_) => "unsupported method receiver".into(),
    })
}

fn parse_timestamp_arg(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        Some(Value::Str(s)) => parse_rfc3339_to_unix(s),
        _ => Err("timestamp(...) expects unix seconds int or RFC3339 string".into()),
    }
}

fn parse_rfc3339_to_unix(s: &str) -> Result<i64, Box<dyn Error>> {
    let time = s
        .split_once(['T', 't'])
        .map(|(_, time)| time)
        .ok_or("invalid RFC3339 timestamp")?;
    let time_without_zone = time
        .trim_end_matches(['Z', 'z'])
        .split(['+', '-'])
        .next()
        .unwrap_or(time);
    if time_without_zone.contains('.') {
        return Err("fractional seconds are not supported".into());
    }
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| format!("invalid RFC3339 timestamp: {e}"))?
        .timestamp())
}

fn expect_string(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(s)) => Ok(s),
        _ => Err("expected string argument".into()),
    }
}

fn expect_int(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        _ => Err("expected int argument".into()),
    }
}

fn expect_map(v: Option<&Value>) -> Result<&BTreeMap<String, Value>, Box<dyn Error>> {
    match v {
        Some(Value::Map(m)) => Ok(m),
        _ => Err("expected map argument".into()),
    }
}

fn expect_array(v: Option<&Value>) -> Result<&Vec<Value>, Box<dyn Error>> {
    match v {
        Some(Value::Array(a)) => Ok(a),
        _ => Err("expected array argument".into()),
    }
}

fn expect_doc_path(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(s)) => {
            validate_doc_path(s)?;
            Ok(s)
        }
        Some(Value::DocRef(d)) => {
            // DocRef は通常 doc() / col().doc() で作られ、その時点で検証済み。
            // 外部から渡された値でも同じ runtime 制約を適用する。
            validate_doc_path(&d.path)?;
            Ok(&d.path)
        }
        _ => Err("expected document path string or doc object".into()),
    }
}

/// 単一 ID 引数(`col().doc()` / `col_group()`)を検証する。
fn validate_document_id(id: &str) -> Result<(), Box<dyn Error>> {
    validate_standalone_id(id, "document id")
}

fn validate_collection_group_id(id: &str) -> Result<(), Box<dyn Error>> {
    validate_standalone_id(id, "collection group id")
}

fn validate_standalone_id(id: &str, kind: &str) -> Result<(), Box<dyn Error>> {
    // 空文字と `/` は既存の error format を保ちつつ、他の ID 制約も同じ
    // runtime エラーにする。
    if id.is_empty() || id.contains('/') {
        return Err(format!("invalid {kind}: {id}").into());
    }
    if let Err(reason) = validate_firestore_id(id) {
        return Err(format!("invalid {kind}: {id} ({reason})").into());
    }
    Ok(())
}

/// ドキュメントパスは偶数個・コレクションパスは奇数個の非空セグメントで
/// 構成されていなければならない(`users/1`、`teams/t1/members`)。
/// 各セグメントは Firestore の document / collection ID 制約も満たす。
/// runtime 層で検証するため、in-memory / emulator のどちらのバックエンド
/// でも同じエラーになる。
fn validate_doc_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, false)
}

fn validate_collection_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, true)
}

fn validate_path(path: &str, collection: bool) -> Result<(), Box<dyn Error>> {
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("invalid path: {path} (empty path segment)").into());
    }
    let parity_ok = segments.len() % 2 == if collection { 1 } else { 0 };
    if !parity_ok {
        let kind = if collection { "collection" } else { "document" };
        let expected = if collection { "odd" } else { "even" };
        return Err(format!(
            "invalid {kind} path: {path} (expected an {expected} number of segments)"
        )
        .into());
    }

    let path_kind = if collection { "collection" } else { "document" };
    for (index, segment) in segments.iter().enumerate() {
        // 偶数番目は collection ID、奇数番目は document ID。ID 制約自体は
        // 両者で同一(spec §6)だが、エラー文でどのセグメントが不正かを
        // 区別するため id_kind は残す。
        let id_kind = if index % 2 == 0 {
            "collection"
        } else {
            "document"
        };
        if let Err(reason) = validate_firestore_id(segment) {
            return Err(format!(
                "invalid {path_kind} path: {path} (segment {segment:?} is an invalid {id_kind} id: {reason})"
            )
            .into());
        }
    }
    Ok(())
}

fn check_block_arity(block: &Block, yielded: usize) -> Result<(), Box<dyn Error>> {
    if block.params.len() != yielded {
        return Err(format!(
            "block expects {} argument(s), got {}",
            block.params.len(),
            yielded
        )
        .into());
    }
    Ok(())
}

/// バックエンドが page 処理済みの候補に相対パスの `__name__` を付ける。
fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

/// Convert an array index to the host index type. Generic over `T` so a test
/// can instantiate a narrower target width on a 64-bit host and exercise the
/// real `TryFrom` conversion rather than a test-only limit check. Production
/// always uses `usize`.
fn checked_array_index<T: TryFrom<i64>>(index: i64) -> Option<T> {
    T::try_from(index).ok()
}

/// Reject values that carry Auth secrets before they can reach any backend.
///
/// Only the secret-bearing Auth variants are checked here.  Broader
/// "writable value" validation (issue #67) is deliberately left to the
/// Firestore layer so this check stays a small, security-focused guard.
fn reject_non_document_value(field: &str, value: &Value) -> Result<(), Box<dyn Error>> {
    match value {
        Value::AuthSession(_) => Err(format!(
            "cannot write auth session to field {field:?}: auth sessions are runtime-only values"
        )
        .into()),
        Value::RedactedToken(_) => Err(format!(
            "cannot write auth token to field {field:?}: auth tokens are runtime-only values"
        )
        .into()),
        Value::Module(module) => Err(format!(
            "cannot write module value to field {field:?}: {} is runtime-only",
            module.name()
        )
        .into()),
        Value::Array(items) | Value::ArrayUnion(items) | Value::ArrayRemove(items) => items
            .iter()
            .try_for_each(|item| reject_non_document_value(field, item)),
        Value::Map(map) => map
            .values()
            .try_for_each(|item| reject_non_document_value(field, item)),
        _ => Ok(()),
    }
}

fn eval_index(base: Value, index: Value) -> Result<Value, Box<dyn Error>> {
    match (base, index) {
        (Value::Map(map), Value::Str(key)) => Ok(map.get(&key).cloned().unwrap_or(Value::Nil)),
        (Value::AuthSession(session), Value::Str(key)) => Ok(auth_session_field(&session, &key)),
        (Value::Array(items), Value::Int(i)) => {
            let index = match checked_array_index::<usize>(i) {
                Some(index) => index,
                None => return Ok(Value::Nil),
            };
            Ok(items.get(index).cloned().unwrap_or(Value::Nil))
        }
        (Value::Str(s), Value::Int(i)) => {
            let index = match checked_array_index::<usize>(i) {
                Some(index) => index,
                None => return Ok(Value::Nil),
            };
            Ok(s.chars()
                .nth(index)
                .map(|c| Value::Str(c.to_string()))
                .unwrap_or(Value::Nil))
        }
        (Value::Str(_), _) => Err("string index must be an int".into()),
        (Value::Map(_), _) => Err("map index must be a string".into()),
        (Value::Array(_), _) => Err("array index must be an int".into()),
        _ => Err("unsupported index receiver".into()),
    }
}

fn eval_binary(l: Value, op: BinOp, r: Value) -> Result<Value, Box<dyn Error>> {
    Ok(match op {
        BinOp::Add => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_add(b))?,
            (Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}")),
            (Value::Array(mut a), Value::Array(b)) => {
                a.extend(b);
                Value::Array(a)
            }
            _ => return Err("unsupported + operands".into()),
        },
        BinOp::Sub => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_sub(b))?,
            (Value::Timestamp(a), Value::Timestamp(b)) => {
                int_binop(a.seconds, b.seconds, |a, b| a.checked_sub(b))?
            }
            _ => return Err("unsupported - operands".into()),
        },
        BinOp::Mul => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_mul(b))?,
            _ => return Err("unsupported * operands".into()),
        },
        BinOp::Div => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                int_binop(a, b, |a, b| a.checked_div(b))?
            }
            _ => return Err("unsupported / operands".into()),
        },
        BinOp::Rem => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                int_binop(a, b, |a, b| a.checked_rem(b))?
            }
            _ => return Err("unsupported % operands".into()),
        },
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::Gt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a > b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds > b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a > b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a > b),
            _ => return Err("unsupported > operands".into()),
        },
        BinOp::Lt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds < b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a < b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a < b),
            _ => return Err("unsupported < operands".into()),
        },
        BinOp::Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds >= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a >= b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a >= b),
            _ => return Err("unsupported >= operands".into()),
        },
        BinOp::Le => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds <= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a <= b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a <= b),
            _ => return Err("unsupported <= operands".into()),
        },
        // `eval_expr` が `And` / `Or` を短絡評価して返すため、ここには到達しない。
        BinOp::And | BinOp::Or => unreachable!("And/Or are short-circuited in eval_expr"),
    })
}

fn int_binop(a: i64, b: i64, f: impl Fn(i64, i64) -> Option<i64>) -> Result<Value, Box<dyn Error>> {
    f(a, b)
        .map(Value::Int)
        .ok_or_else(|| "integer overflow".into())
}

/// 予算を診断文に出すための表記。
///
/// ミリ秒固定だと 500µs のような sub-millisecond 予算が "max 0ms" と
/// 誤表示され、単位も分からない。`Duration` の `Debug` 表現は設定した
/// 単位に合わせて出るため、予算と診断文がずれない。
fn format_duration(duration: Duration) -> String {
    format!("{duration:?}")
}

fn backend_err(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FlareError>().is_some() {
        e
    } else {
        FlareError::backend(e.to_string()).into()
    }
}

/// eval 失敗時にトップレベル文の位置を付与する。既に span を持つ
/// FlareError (lex/parse/backendで付与済み)はそのまま通し、
/// span なしの FlareError と String 系エラーは Runtime として包む。
fn attach_span(e: Box<dyn Error>, span: Option<Span>) -> Box<dyn Error> {
    let Some(span) = span else {
        return wrap_as_runtime(e);
    };
    match e.downcast::<CapabilityError>() {
        Ok(mut capability) => {
            if capability.span.is_none() {
                capability.span = Some(span);
            }
            capability as Box<dyn Error>
        }
        Err(e) => match e.downcast::<FlareError>() {
            Ok(mut ferr) => {
                if ferr.span.is_none() {
                    ferr.span = Some(span);
                }
                ferr as Box<dyn Error>
            }
            Err(e) => FlareError::runtime(e.to_string()).with_span(span).into(),
        },
    }
}

fn wrap_as_runtime(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FlareError>().is_some() || e.downcast_ref::<CapabilityError>().is_some() {
        e
    } else {
        FlareError::runtime(e.to_string()).into()
    }
}

enum ExecFlow {
    Value(Value),
    Return(Value),
}

impl ExecFlow {
    fn into_value(self) -> Value {
        match self {
            ExecFlow::Value(v) | ExecFlow::Return(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firestore::InMemoryFirestore;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn array_index_conversion_respects_target_width() {
        // A 64-bit host would accept 2^32 as a usize index, so instantiate the
        // conversion at the narrower width to prove the narrowing is rejected
        // by a real `TryFrom` and not only by a host-specific bound.
        assert_eq!(
            checked_array_index::<u32>(4_294_967_295),
            Some(4_294_967_295)
        );
        assert_eq!(checked_array_index::<u32>(4_294_967_296), None);
        assert_eq!(checked_array_index::<u32>(4_294_967_297), None);
        assert_eq!(checked_array_index::<u32>(-1), None);
        assert_eq!(checked_array_index::<u16>(65_536), None);
        assert_eq!(checked_array_index::<usize>(0), Some(0));
        // The host width still accepts the full i64 range that fits it.
        assert_eq!(
            checked_array_index::<usize>(i64::MAX),
            Some(i64::MAX as usize)
        );
        assert_eq!(checked_array_index::<usize>(-1), None);
    }

    #[test]
    fn builtin_table_matches_name_list() {
        let specs = builtin_specs();
        let names: HashSet<_> = specs.iter().map(|spec| spec.global_name).collect();
        assert_eq!(names.len(), specs.len(), "duplicate builtin names");
        assert!(specs.iter().all(|spec| is_builtin_name(spec.global_name)));
        assert!(!is_builtin_name("undefined_fn"));
    }

    struct FailingCommitFirestore {
        in_tx: bool,
        aborted: bool,
        abort_calls: usize,
        abort_failures_remaining: usize,
    }

    impl Firestore for FailingCommitFirestore {
        fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn run_query(
            &self,
            _query: &QueryRef,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            Ok(vec![])
        }
        fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            if self.in_tx {
                return Err("already in transaction".into());
            }
            self.in_tx = true;
            Ok(())
        }
        fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Err("commit failed".into())
        }
        fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            self.abort_calls += 1;
            if !self.in_tx {
                return Ok(());
            }
            if self.abort_failures_remaining > 0 {
                self.abort_failures_remaining -= 1;
                return Err("abort failed".into());
            }
            self.in_tx = false;
            self.aborted = true;
            Ok(())
        }
    }

    fn empty_transaction_stmt() -> Stmt {
        Stmt::Expr(Expr::Call {
            receiver: None,
            name: "transaction".into(),
            args: vec![],
            block: Some(Block {
                params: vec![],
                body: vec![],
            }),
        })
    }

    fn failing_commit_runtime(abort_failures_remaining: usize) -> Runtime<FailingCommitFirestore> {
        Runtime::new(FailingCommitFirestore {
            in_tx: false,
            aborted: false,
            abort_calls: 0,
            abort_failures_remaining,
        })
    }

    #[test]
    fn transaction_aborts_on_commit_failure() {
        let mut runtime = failing_commit_runtime(0);
        let res = runtime.eval_program(&[empty_transaction_stmt()]);
        assert!(res.is_err());
        assert_eq!(runtime.services.firestore.abort_calls, 1);
        assert!(runtime.services.firestore.aborted);
        assert!(!runtime.services.firestore.in_tx);
    }

    #[test]
    fn transaction_keeps_commit_error_when_abort_also_fails() {
        let mut runtime = failing_commit_runtime(1);
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert_eq!(err.to_string(), "commit failed");
        assert_eq!(runtime.services.firestore.abort_calls, 1);
        // A failed rollback leaves the transaction available for retry.
        assert!(runtime.services.firestore.in_tx);
    }

    #[test]
    fn transaction_keeps_block_error_when_abort_also_fails() {
        let mut runtime = failing_commit_runtime(1);
        let err = runtime.run("transaction { undefined_var }").unwrap_err();
        assert!(err
            .to_string()
            .contains("undefined variable: undefined_var"));
        assert_eq!(runtime.services.firestore.abort_calls, 1);
        assert!(runtime.services.firestore.in_tx);
    }

    #[test]
    fn next_transaction_retries_a_failed_rollback() {
        // Only the first rollback fails, so the retry before the next
        // `transaction` block succeeds and the block reaches commit instead of
        // failing with "already in transaction".
        let mut runtime = failing_commit_runtime(1);
        assert!(runtime.eval_program(&[empty_transaction_stmt()]).is_err());
        assert!(runtime.services.firestore.in_tx);
        assert!(runtime.state.pending_rollback);

        // The fake commit always fails, so this run still errors — but with
        // the commit error, which proves the stale transaction was cleared
        // before `begin_transaction`.
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert_eq!(err.to_string(), "commit failed");
        // Abort calls: the failed transaction's rollback, the retry before the
        // new begin, and the rollback after this commit failed too.
        assert_eq!(runtime.services.firestore.abort_calls, 3);
        assert!(!runtime.services.firestore.in_tx);
    }

    #[test]
    fn transaction_reports_backend_still_busy_when_rollback_keeps_failing() {
        // Every rollback fails, so the retry cannot clear the state. The next
        // block must still surface an honest error instead of succeeding.
        let mut runtime = failing_commit_runtime(usize::MAX);
        assert!(runtime.eval_program(&[empty_transaction_stmt()]).is_err());
        let err = runtime
            .eval_program(&[empty_transaction_stmt()])
            .unwrap_err();
        assert!(err.to_string().contains("already in transaction"), "{err}");
        assert!(runtime.state.pending_rollback);
    }

    #[test]
    fn transaction_abort_can_be_retried() {
        let mut runtime = failing_commit_runtime(1);
        runtime.services.firestore.begin_transaction().unwrap();
        assert_eq!(
            runtime
                .services
                .firestore
                .abort_transaction()
                .unwrap_err()
                .to_string(),
            "abort failed"
        );
        assert!(runtime.services.firestore.in_tx);
        runtime.services.firestore.abort_transaction().unwrap();
        assert!(!runtime.services.firestore.in_tx);
        runtime.services.firestore.begin_transaction().unwrap();
    }

    struct PanicQueryFirestore;

    impl Firestore for PanicQueryFirestore {
        fn get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn apply(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn run_query(
            &self,
            _query: &QueryRef,
        ) -> Result<crate::firestore::QueryCandidates, Box<dyn Error>> {
            panic!("query panic")
        }
        fn begin_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn transaction_get(&self, _path: &str) -> Result<Option<Value>, Box<dyn Error>> {
            Ok(None)
        }
        fn commit_transaction(&mut self, _writes: Vec<WriteOp>) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
        fn abort_transaction(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }
    }

    #[test]
    fn scope_guards_restore_after_binding_error() {
        let mut runtime = Runtime::new(InMemoryFirestore::default());
        runtime.state.scopes.push(std::collections::HashMap::from([(
            "caller".to_string(),
            Value::Int(7),
        )]));

        {
            let mut evaluator = runtime.evaluator();
            let scope = ScopeGuard::function(&mut evaluator);
            scope.runtime.set_var("param", Value::Int(1)).unwrap();
            assert!(scope.runtime.set_var("param", Value::Int(2)).is_err());
        }
        let mut evaluator = runtime.evaluator();
        assert_eq!(evaluator.get_var("caller"), Some(&Value::Int(7)));
        assert!(evaluator.get_var("param").is_none());

        {
            let scope = ScopeGuard::block(&mut evaluator);
            scope.runtime.set_var("item", Value::Int(1)).unwrap();
            assert!(scope.runtime.set_var("item", Value::Int(2)).is_err());
        }
        assert_eq!(evaluator.get_var("caller"), Some(&Value::Int(7)));
        assert!(evaluator.get_var("item").is_none());
        evaluator.scopes.pop();
    }

    #[test]
    fn scope_guards_restore_after_panic_unwind() {
        let mut runtime = Runtime::new(PanicQueryFirestore);

        let result = catch_unwind(AssertUnwindSafe(|| {
            runtime.run("fn f(param) { col(\"things\").get() }\nf(1)")
        }));
        assert!(result.is_err());
        assert!(runtime.run("param").is_err());

        let result = catch_unwind(AssertUnwindSafe(|| {
            runtime.run("[1].map { |item| col(\"things\").get() }")
        }));
        assert!(result.is_err());
        assert!(runtime.run("item").is_err());
    }
}
