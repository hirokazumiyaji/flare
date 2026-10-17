use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    UndefinedVariable(String),
    UndefinedFunction(String),
    ReassignmentNotAllowed(String),
    CannotRedefineBuiltin(String),
    TooManyPositionalArgs,
    MissingArgument(String),
    DuplicateArgument(String),
    UnknownKeywordArguments(Vec<String>),
    DuplicateKeywordArgument(String),
    DuplicateParam { kind: String, name: String },
    BlockNotAllowed(String),
    RequiresBlock(String),
    BlockTakesNoParameters(String),
    WrongBlockArity { expected: usize, got: usize },
    PositionalArityMismatch {
        name: String,
        expected: usize,
        actual: usize,
        exact: bool,
        hint: Option<&'static str>,
    },
    UnsupportedMethodReceiver,
    UnknownMethod { receiver: String, name: String },
    NestedStaging(String),
    QueryInBatch,
    QueryInTransaction,
    GetInBatch,
    GetAfterWriteInTransaction,
    StorageInBatch,
    StorageInTransaction,
    StepLimitExceeded(u64),
    LoopIterationLimitExceeded(u64),
    CallDepthLimitExceeded(usize),
    WallClockLimitExceeded(String),
    StringSizeLimitExceeded(usize),
    BytesSizeLimitExceeded(usize),
    QueryResultLimitExceeded(usize),
    IntegerOverflow,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndefinedVariable(name) => write!(f, "undefined variable: {name}"),
            Self::UndefinedFunction(name) => write!(f, "undefined function: {name}"),
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
            Self::NestedStaging(name) => write!(f, "nested {name} is not allowed"),
            Self::QueryInBatch => write!(f, "queries are not allowed inside batch"),
            Self::QueryInTransaction => {
                write!(f, "queries are not allowed inside transaction")
            }
            Self::GetInBatch => write!(f, "get is not allowed inside batch"),
            Self::GetAfterWriteInTransaction => {
                write!(f, "get is not allowed after the first write in a transaction")
            }
            Self::StorageInBatch => write!(f, "storage APIs are not allowed inside batch"),
            Self::StorageInTransaction => {
                write!(f, "storage APIs are not allowed inside transaction")
            }
            Self::StepLimitExceeded(max) => write!(f, "step limit exceeded (max {max})"),
            Self::LoopIterationLimitExceeded(max) => {
                write!(f, "loop iteration limit exceeded (max {max})")
            }
            Self::CallDepthLimitExceeded(max) => {
                write!(f, "call depth limit exceeded (max {max})")
            }
            Self::WallClockLimitExceeded(max) => {
                write!(f, "wall-clock limit exceeded (max {max})")
            }
            Self::StringSizeLimitExceeded(max) => {
                write!(f, "string size limit exceeded (max {max})")
            }
            Self::BytesSizeLimitExceeded(max) => {
                write!(f, "bytes size limit exceeded (max {max})")
            }
            Self::QueryResultLimitExceeded(max) => {
                write!(f, "query result limit exceeded (max {max})")
            }
            Self::IntegerOverflow => write!(f, "integer overflow"),
        }
    }
}

impl Error for RuntimeError {}
