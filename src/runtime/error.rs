use std::error::Error;
use std::fmt;

/// Typed runtime errors raised by the evaluator, the call contracts and the
/// method/builtin dispatchers.
///
/// These are the errors a script can observe as ordinary runtime failures.
/// Engine-enforced budget violations (step/loop/call-depth/wall-clock/value and
/// query limits) are deliberately *not* modelled here: they stay
/// `crate::error::BudgetError` so that `try / rescue` cannot swallow them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    UndefinedVariable(String),
    UndefinedFunction(String),
    CannotCallNonFunction(String),
    CannotCallMethodOnNil(String),
    CannotCallMethodOnType {
        name: String,
        type_name: String,
    },
    ReassignmentNotAllowed(String),
    CannotRedefineBuiltin(String),
    TooManyPositionalArgs,
    MissingArgument(String),
    DuplicateArgument(String),
    UnknownKeywordArguments(Vec<String>),
    DuplicateKeywordArgument(String),
    DuplicateParam {
        kind: String,
        name: String,
    },
    BlockNotAllowed(String),
    RequiresBlock(String),
    BlockTakesNoParameters(String),
    WrongBlockArity {
        expected: usize,
        got: usize,
    },
    PositionalArityMismatch {
        name: String,
        expected: usize,
        actual: usize,
        exact: bool,
        hint: Option<&'static str>,
    },
    UnsupportedMethodReceiver,
    UnknownMethod {
        receiver: String,
        name: String,
    },
    UnknownModuleMethod {
        module: String,
        name: String,
    },
    UnsupportedFieldAccess,
    NestedStaging(String),
    QueryInBatch,
    QueryInTransaction,
    GetInBatch,
    GetAfterWriteInTransaction,
    StorageInBatch,
    StorageInTransaction,
    IntegerOverflow,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndefinedVariable(name) => write!(f, "undefined variable: {name}"),
            Self::UndefinedFunction(name) => write!(f, "undefined function: {name}"),
            Self::CannotCallNonFunction(type_name) => {
                write!(f, "cannot call non-function value of type {type_name}")
            }
            Self::CannotCallMethodOnNil(name) => {
                write!(f, "cannot call method '{name}' on nil")
            }
            Self::CannotCallMethodOnType { name, type_name } => {
                write!(f, "cannot call method '{name}' on {type_name}")
            }
            Self::ReassignmentNotAllowed(name) => write!(f, "reassignment is not allowed: {name}"),
            Self::CannotRedefineBuiltin(name) => write!(f, "cannot redefine builtin: {name}"),
            Self::TooManyPositionalArgs => write!(f, "too many positional args"),
            Self::MissingArgument(name) => write!(f, "missing argument: {name}"),
            Self::DuplicateArgument(name) => write!(
                f,
                "duplicate argument {name}: specified both positionally and as a keyword"
            ),
            Self::UnknownKeywordArguments(keys) => {
                write!(f, "unknown keyword arguments: {}", keys.join(", "))
            }
            Self::DuplicateKeywordArgument(name) => {
                write!(f, "duplicate keyword argument: {name}")
            }
            Self::DuplicateParam { kind, name } => {
                write!(f, "duplicate {kind} parameter: {name}")
            }
            Self::BlockNotAllowed(name) => {
                write!(f, "{name} does not take a block (block is not allowed)")
            }
            Self::RequiresBlock(name) => write!(f, "{name} requires {{ ... }} block"),
            Self::BlockTakesNoParameters(name) => {
                write!(f, "{name} block does not take parameters")
            }
            Self::WrongBlockArity { expected, got } => {
                write!(f, "block expects {expected} argument(s), got {got}")
            }
            Self::PositionalArityMismatch {
                name,
                expected,
                actual,
                exact,
                hint,
            } => {
                if *expected == 0 {
                    write!(
                        f,
                        "{name} does not take arguments: too many positional arguments (expected 0, got {actual})"
                    )
                } else if *actual < *expected {
                    let h = hint.map(|h| format!(" ({h})")).unwrap_or_default();
                    write!(
                        f,
                        "{name} is missing positional arguments: expected {expected}, got {actual}{h}"
                    )
                } else {
                    let expectation = if *exact { "exactly" } else { "at most" };
                    write!(
                        f,
                        "too many positional arguments for {name} (expected {expectation} {expected}, got {actual})"
                    )
                }
            }
            Self::UnsupportedMethodReceiver => write!(f, "unsupported method receiver"),
            Self::UnknownMethod { receiver, name } => {
                write!(f, "unknown {receiver} method: {name}")
            }
            Self::UnknownModuleMethod { module, name } => {
                write!(f, "unknown module method: {module}.{name}")
            }
            Self::UnsupportedFieldAccess => write!(f, "field access is only supported on map"),
            Self::NestedStaging(name) => write!(f, "nested {name} is not allowed"),
            Self::QueryInBatch => write!(f, "queries are not allowed inside batch"),
            Self::QueryInTransaction => {
                write!(f, "queries are not allowed inside transaction")
            }
            Self::GetInBatch => write!(f, "get is not allowed inside batch"),
            Self::GetAfterWriteInTransaction => {
                write!(
                    f,
                    "get is not allowed after the first write in a transaction"
                )
            }
            Self::StorageInBatch => write!(f, "storage APIs are not allowed inside batch"),
            Self::StorageInTransaction => {
                write!(f, "storage APIs are not allowed inside transaction")
            }
            Self::IntegerOverflow => write!(f, "integer overflow"),
        }
    }
}

impl Error for RuntimeError {}
