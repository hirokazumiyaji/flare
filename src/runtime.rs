#[cfg(test)]
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::time::{Duration, Instant};

use crate::ast::{BinOp, Block, Expr, Stmt};
use crate::auth::Auth;
use crate::error::{BudgetError, CapabilityError, FlareError, ServiceId, Span, UserRaisedError};
use crate::firestore::{validate_firestore_id, Firestore, WriteOp};
use crate::limits::{RuntimeLimits, RuntimeUsage};
use crate::parser::Parser;
use crate::services::ServiceAccess;
use crate::services::Services;
use crate::storage::{InMemoryStorage, Storage};
#[cfg(test)]
use crate::value::QueryRef;
use crate::value::Value;

pub(crate) mod builtins;
pub(crate) mod contract;
pub(crate) mod error;
mod evaluator;
pub(crate) mod methods;
mod state;

use builtins::auth::auth_session_field;
#[cfg(test)]
use builtins::{builtin_specs, is_builtin_name};
use evaluator::Evaluator;
#[cfg(test)]
use evaluator::ScopeGuard;
use state::RuntimeState;

pub(crate) enum ExecCtx {
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
        services.storage.set_max_bytes_size(limits.max_bytes_size);
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
        self.services
            .storage
            .set_max_bytes_size(limits.max_bytes_size);
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

/// `bytes(...)` の引数エラー文言。3 箇所から使うので定数にまとめる。
const BYTES_USAGE: &str = "bytes expects one string or an array of integers from 0 to 255";
const BYTES_ELEMENT_ERR: &str = "bytes array elements must be integers from 0 to 255";

pub(crate) fn parse_timestamp_arg(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        Some(Value::Str(s)) => parse_rfc3339_to_unix(s),
        _ => Err("timestamp(...) expects unix seconds int or RFC3339 string".into()),
    }
}

pub(crate) fn parse_rfc3339_to_unix(s: &str) -> Result<i64, Box<dyn Error>> {
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

pub(crate) fn expect_string(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
    match v {
        Some(Value::Str(s)) => Ok(s),
        _ => Err("expected string argument".into()),
    }
}

pub(crate) fn expect_int(v: Option<&Value>) -> Result<i64, Box<dyn Error>> {
    match v {
        Some(Value::Int(i)) => Ok(*i),
        _ => Err("expected int argument".into()),
    }
}

pub(crate) fn expect_map(v: Option<&Value>) -> Result<&BTreeMap<String, Value>, Box<dyn Error>> {
    match v {
        Some(Value::Map(m)) => Ok(m),
        _ => Err("expected map argument".into()),
    }
}

pub(crate) fn expect_array(v: Option<&Value>) -> Result<&Vec<Value>, Box<dyn Error>> {
    match v {
        Some(Value::Array(a)) => Ok(a),
        _ => Err("expected array argument".into()),
    }
}

pub(crate) fn expect_doc_path(v: Option<&Value>) -> Result<&str, Box<dyn Error>> {
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
pub(crate) fn validate_document_id(id: &str) -> Result<(), Box<dyn Error>> {
    validate_standalone_id(id, "document id")
}

pub(crate) fn validate_collection_group_id(id: &str) -> Result<(), Box<dyn Error>> {
    validate_standalone_id(id, "collection group id")
}

pub(crate) fn validate_standalone_id(id: &str, kind: &str) -> Result<(), Box<dyn Error>> {
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
pub(crate) fn validate_doc_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, false)
}

pub(crate) fn validate_collection_path(path: &str) -> Result<(), Box<dyn Error>> {
    validate_path(path, true)
}

pub(crate) fn validate_path(path: &str, collection: bool) -> Result<(), Box<dyn Error>> {
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

/// バックエンドが page 処理済みの候補に相対パスの `__name__` を付ける。
pub(crate) fn wrap_doc(path: &str, doc: &BTreeMap<String, Value>) -> Value {
    let mut out = doc.clone();
    out.insert("__name__".to_string(), Value::Str(path.to_string()));
    Value::Map(out)
}

/// Convert an array index to the host index type. Generic over `T` so a test
/// can instantiate a narrower target width on a 64-bit host and exercise the
/// real `TryFrom` conversion rather than a test-only limit check. Production
/// always uses `usize`.
pub(crate) fn checked_array_index<T: TryFrom<i64>>(index: i64) -> Option<T> {
    T::try_from(index).ok()
}

/// Reject values that carry Auth secrets before they can reach any backend.
///
/// Only the secret-bearing Auth variants are checked here.  Broader
/// "writable value" validation (issue #67) is deliberately left to the
/// Firestore layer so this check stays a small, security-focused guard.
pub(crate) fn reject_non_document_value(field: &str, value: &Value) -> Result<(), Box<dyn Error>> {
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

pub(crate) fn eval_index(base: Value, index: Value) -> Result<Value, Box<dyn Error>> {
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

pub(crate) fn eval_binary(l: Value, op: BinOp, r: Value) -> Result<Value, Box<dyn Error>> {
    Ok(match op {
        BinOp::Add => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_add(b))?,
            (Value::Str(a), Value::Str(b)) => Value::Str(format!("{a}{b}")),
            (Value::Array(mut a), Value::Array(b)) => {
                a.extend(b);
                Value::Array(a)
            }
            (l, r) => {
                return Err(format!(
                    "unsupported operands for +: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Sub => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_sub(b))?,
            (Value::Timestamp(a), Value::Timestamp(b)) => {
                int_binop(a.seconds, b.seconds, |a, b| a.checked_sub(b))?
            }
            (l, r) => {
                return Err(format!(
                    "unsupported operands for -: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Mul => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binop(a, b, |a, b| a.checked_mul(b))?,
            (l, r) => {
                return Err(format!(
                    "unsupported operands for *: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Div => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                int_binop(a, b, |a, b| a.checked_div(b))?
            }
            (l, r) => {
                return Err(format!(
                    "unsupported operands for /: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Rem => match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                int_binop(a, b, |a, b| a.checked_rem(b))?
            }
            (l, r) => {
                return Err(format!(
                    "unsupported operands for %: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::Gt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a > b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds > b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a > b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a > b),
            (l, r) => {
                return Err(format!(
                    "unsupported operands for >: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Lt => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a < b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds < b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a < b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a < b),
            (l, r) => {
                return Err(format!(
                    "unsupported operands for <: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Ge => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a >= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds >= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a >= b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a >= b),
            (l, r) => {
                return Err(format!(
                    "unsupported operands for >=: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        BinOp::Le => match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Bool(a <= b),
            (Value::Timestamp(a), Value::Timestamp(b)) => Value::Bool(a.seconds <= b.seconds),
            (Value::Bytes(a), Value::Bytes(b)) => Value::Bool(a <= b),
            (Value::Str(a), Value::Str(b)) => Value::Bool(a <= b),
            (l, r) => {
                return Err(format!(
                    "unsupported operands for <=: {} and {}",
                    l.type_name(),
                    r.type_name()
                )
                .into())
            }
        },
        // `eval_expr` が `And` / `Or` を短絡評価して返すため、ここには到達しない。
        BinOp::And | BinOp::Or => unreachable!("And/Or are short-circuited in eval_expr"),
    })
}

pub(crate) fn int_binop(
    a: i64,
    b: i64,
    f: impl Fn(i64, i64) -> Option<i64>,
) -> Result<Value, Box<dyn Error>> {
    f(a, b)
        .map(Value::Int)
        .ok_or_else(|| "integer overflow".into())
}

/// 予算を診断文に出すための表記。
///
/// ミリ秒固定だと 500µs のような sub-millisecond 予算が "max 0ms" と
/// 誤表示され、単位も分からない。`Duration` の `Debug` 表現は設定した
/// 単位に合わせて出るため、予算と診断文がずれない。
pub(crate) fn format_duration(duration: Duration) -> String {
    format!("{duration:?}")
}

pub(crate) fn backend_err(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FlareError>().is_some() {
        e
    } else {
        FlareError::backend(e.to_string()).into()
    }
}

/// eval 失敗時にトップレベル文の位置を付与する。既に span を持つ
/// FlareError (lex/parse/backendで付与済み)はそのまま通し、
/// eval 失敗時にトップレベル文の位置を付与する。既に span を持つ
/// FlareError (lex/parse/backendで付与済み)はそのまま通し、
/// span なしの FlareError と String 系エラーは Runtime として包む。
pub(crate) fn attach_span(e: Box<dyn Error>, span: Option<Span>) -> Box<dyn Error> {
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
            Err(e) => match e.downcast::<UserRaisedError>() {
                Ok(uerr) => {
                    let err_span = uerr.span.or(Some(span));
                    FlareError::runtime(uerr.message)
                        .with_span(err_span.unwrap())
                        .into()
                }
                Err(e) => match e.downcast::<BudgetError>() {
                    Ok(berr) => {
                        let err_span = berr.span.or(Some(span));
                        let mut ferr = FlareError::runtime(berr.message);
                        ferr.span = err_span;
                        ferr.frames = berr.frames;
                        ferr.into()
                    }
                    Err(e) => FlareError::runtime(e.to_string()).with_span(span).into(),
                },
            },
        },
    }
}

pub(crate) fn attach_expr_span(e: Box<dyn Error>, span: Span) -> Box<dyn Error> {
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
            Err(e) => match e.downcast::<BudgetError>() {
                Ok(mut berr) => {
                    if berr.span.is_none() {
                        berr.span = Some(span);
                    }
                    berr as Box<dyn Error>
                }
                Err(e) => match e.downcast::<UserRaisedError>() {
                    Ok(mut uerr) => {
                        if uerr.span.is_none() {
                            uerr.span = Some(span);
                        }
                        uerr as Box<dyn Error>
                    }
                    Err(e) => FlareError::runtime(e.to_string()).with_span(span).into(),
                },
            },
        },
    }
}

pub(crate) fn wrap_as_runtime(e: Box<dyn Error>) -> Box<dyn Error> {
    if e.downcast_ref::<FlareError>().is_some() || e.downcast_ref::<CapabilityError>().is_some() {
        e
    } else if let Some(uerr) = e.downcast_ref::<UserRaisedError>() {
        let mut ferr = FlareError::runtime(uerr.message.clone());
        ferr.span = uerr.span;
        ferr.into()
    } else if let Some(berr) = e.downcast_ref::<BudgetError>() {
        let mut ferr = FlareError::runtime(berr.message.clone());
        ferr.span = berr.span;
        ferr.frames = berr.frames.clone();
        ferr.into()
    } else {
        FlareError::runtime(e.to_string()).into()
    }
}

pub(crate) enum ExecFlow {
    Value(Value),
    Return(Value),
}

impl ExecFlow {
    pub(crate) fn into_value(self) -> Value {
        match self {
            ExecFlow::Value(v) | ExecFlow::Return(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ExprKind;
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
        Stmt::Expr(Expr::new(
            ExprKind::Call {
                receiver: None,
                name: "transaction".into(),
                args: vec![],
                block: Some(Block {
                    params: vec![],
                    body: vec![],
                }),
            },
            Span::new(1, 1),
        ))
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
        assert!(err.to_string().contains("commit failed"));
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
        assert!(err.to_string().contains("commit failed"));
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
