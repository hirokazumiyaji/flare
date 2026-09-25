use std::error::Error;
use std::fmt;

use crate::auth::AuthErrorKind;

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
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Lex => "lex",
            Self::Parse => "parse",
            Self::Runtime => "runtime",
            Self::Backend => "backend",
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
    /// Present when a backend error originated in the Auth adapter.  Keeping
    /// the category alongside the existing generic Backend kind lets library
    /// callers make typed decisions while preserving the CLI's line/column
    /// error format.
    pub auth_kind: Option<AuthErrorKind>,
}

impl FirelangError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
            auth_kind: None,
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

    pub fn auth(kind: AuthErrorKind, message: impl Into<String>) -> Self {
        let mut error = Self::backend(message);
        error.auth_kind = Some(kind);
        error
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
