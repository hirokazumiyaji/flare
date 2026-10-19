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

/// A call frame in a user function stack trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrame {
    pub name: String,
    pub span: Span,
}

impl CallFrame {
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }
}

impl fmt::Display for CallFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "in {} at {}", self.name, self.span)
    }
}

/// Typed flare error with optional source span.
///
/// Display is `line:col: message` when a span is present, otherwise `message`.
/// When user function call frames are present, they are printed indented on subsequent lines.
#[derive(Debug, Clone, PartialEq)]
pub struct FlareError {
    pub kind: ErrorKind,
    pub message: String,
    pub span: Option<Span>,
    /// Present when a backend error originated in the Auth adapter.  Keeping
    /// the category alongside the existing generic Backend kind lets library
    /// callers make typed decisions while preserving the CLI's line/column
    /// error format.
    pub auth_kind: Option<AuthErrorKind>,
    pub frames: Vec<CallFrame>,
}

impl FlareError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
            auth_kind: None,
            frames: Vec::new(),
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn push_frame(&mut self, name: impl Into<String>, span: Span) {
        self.frames.push(CallFrame::new(name, span));
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

    pub fn runtime_with_span(message: impl Into<String>, span: Span) -> Self {
        Self::new(ErrorKind::Runtime, message).with_span(span)
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

impl fmt::Display for FlareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = self.span {
            write!(f, "{span}: {}", self.message)?;
        } else {
            write!(f, "{}", self.message)?;
        }
        for frame in &self.frames {
            write!(f, "\n  {frame}")?;
        }
        Ok(())
    }
}

impl Error for FlareError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceId {
    Firestore,
    Auth,
    Storage,
    IO,
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Firestore => "Firestore",
            Self::Auth => "Auth",
            Self::Storage => "Storage",
            Self::IO => "IO",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityError {
    pub service: ServiceId,
    pub span: Option<Span>,
    pub frames: Vec<CallFrame>,
}

impl CapabilityError {
    pub fn new(service: ServiceId) -> Self {
        Self {
            service,
            span: None,
            frames: Vec::new(),
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn push_frame(&mut self, name: impl Into<String>, span: Span) {
        self.frames.push(CallFrame::new(name, span));
    }

    pub fn kind(&self) -> ErrorKind {
        ErrorKind::Runtime
    }
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(span) = self.span {
            write!(f, "{span}: ")?;
        }
        write!(f, "service unavailable: {}", self.service)?;
        for frame in &self.frames {
            write!(f, "\n  {frame}")?;
        }
        Ok(())
    }
}

impl Error for CapabilityError {}

/// An error explicitly raised by user code via `raise(msg)`.
///
/// Distinguishing user-raised errors from engine/budget limit errors ensures
/// that `try / rescue` can always catch user errors, even if the message
/// contains phrases like `"step limit exceeded"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRaisedError {
    pub message: String,
    pub span: Option<Span>,
}

impl UserRaisedError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }
}

impl fmt::Display for UserRaisedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(f, "{span}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl Error for UserRaisedError {}

/// An engine-enforced budget or resource limit error.
///
/// Budget limit errors (e.g. step count, wall-clock time, call depth, memory/value nesting)
/// bypass `try / rescue` blocks and propagate to terminate script execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetError {
    pub message: String,
    pub span: Option<Span>,
    pub frames: Vec<CallFrame>,
}

impl BudgetError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            frames: Vec::new(),
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn push_frame(&mut self, name: impl Into<String>, span: Span) {
        self.frames.push(CallFrame {
            name: name.into(),
            span,
        });
    }
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(span) => write!(f, "{span}: {}", self.message)?,
            None => write!(f, "{}", self.message)?,
        }
        for frame in &self.frames {
            write!(f, "\n  {frame}")?;
        }
        Ok(())
    }
}

impl Error for BudgetError {}

/// Add a call frame to an error, preserving typed FlareError, CapabilityError, and BudgetError.
pub fn add_call_frame(e: Box<dyn Error>, name: impl Into<String>, span: Span) -> Box<dyn Error> {
    match e.downcast::<FlareError>() {
        Ok(mut ferr) => {
            ferr.push_frame(name, span);
            ferr as Box<dyn Error>
        }
        Err(e) => match e.downcast::<CapabilityError>() {
            Ok(mut cap) => {
                cap.push_frame(name, span);
                cap as Box<dyn Error>
            }
            Err(e) => match e.downcast::<BudgetError>() {
                Ok(mut berr) => {
                    berr.push_frame(name, span);
                    berr as Box<dyn Error>
                }
                Err(e) => match e.downcast::<UserRaisedError>() {
                    Ok(uerr) => {
                        let mut ferr = FlareError::runtime(uerr.message);
                        if let Some(span) = uerr.span {
                            ferr = ferr.with_span(span);
                        }
                        ferr.push_frame(name, span);
                        ferr.into()
                    }
                    Err(e) => {
                        let mut ferr = FlareError::runtime(e.to_string());
                        ferr.push_frame(name, span);
                        ferr.into()
                    }
                },
            },
        },
    }
}
