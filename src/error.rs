use std::error::Error;
use std::fmt;

/// Source position (1-based line and column, in characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Error phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Lex,
    Parse,
    Runtime,
    Backend,
    /// A service was not configured for the requested operation.
    ///
    /// This is distinct from a backend failure: a capability error means the
    /// embedding application did not provide Auth, Storage, or Firestore to
    /// the `RuntimeContext`.
    Capability,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Lex => "lex",
            Self::Parse => "parse",
            Self::Runtime => "runtime",
            Self::Backend => "backend",
            Self::Capability => "capability",
        };
        write!(f, "{s}")
    }
}

/// Typed firelang error with optional source span.
///
/// Display is `line:col: message` when a span is present, otherwise `message`.
/// This keeps existing `contains("...")` assertions working while adding
/// positions for CLI output (`error: <display>`).
#[derive(Debug, Clone, PartialEq)]
pub struct FirelangError {
    pub kind: ErrorKind,
    pub message: String,
    pub span: Option<Span>,
}

impl FirelangError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn lex(message: impl Into<String>, line: usize, col: usize) -> Self {
        Self::new(ErrorKind::Lex, message).with_span(Span::new(line, col))
    }

    pub fn parse(message: impl Into<String>, span: Option<Span>) -> Self {
        let mut e = Self::new(ErrorKind::Parse, message);
        e.span = span;
        e
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message)
    }

    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Backend, message)
    }

    /// Construct the stable error used when a service was not supplied to a
    /// `RuntimeContext`.
    pub fn capability(service: impl Into<String>, operation: impl Into<String>) -> Self {
        Self::new(
            ErrorKind::Capability,
            CapabilityError::new(service, operation).to_string(),
        )
    }
}

impl fmt::Display for FirelangError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(f, "{span}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl Error for FirelangError {}

impl From<CapabilityError> for FirelangError {
    fn from(error: CapabilityError) -> Self {
        Self::new(ErrorKind::Capability, error.to_string())
    }
}

/// Structured form of a missing-service error.
///
/// `FirelangError::capability` is used at runtime so source spans can be
/// attached just like other Firelang errors.  This value is public as well so
/// embedders can construct and test capability failures without parsing a
/// message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityError {
    pub service: String,
    pub operation: String,
}

impl CapabilityError {
    pub fn new(service: impl Into<String>, operation: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            operation: operation.into(),
        }
    }
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "capability unavailable: {} ({})",
            self.service, self.operation
        )
    }
}

impl Error for CapabilityError {}
