use std::fmt;

use crate::error::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Str(String),
    StrInterp(Vec<StrPiece>),
    Bool(bool),
    Nil,
    Var(String),
    Binary {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Call {
        receiver: Option<Box<Expr>>,
        name: String,
        /// Positional and keyword arguments stay in source order. Binding
        /// them to parameters is a separate step performed by the runtime.
        args: Vec<CallArg>,
        block: Option<Block>,
    },
    FieldAccess {
        receiver: Box<Expr>,
        name: String,
    },
    Map(Vec<(String, Expr)>),
    Array(Vec<Expr>),
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    If {
        cond: Box<Expr>,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        cond: Box<Expr>,
        body: Vec<Stmt>,
    },
    TryRescue {
        try_body: Vec<Stmt>,
        err_param: Option<String>,
        rescue_body: Vec<Stmt>,
    },
    Fn {
        params: Vec<FuncParam>,
        body: Vec<Stmt>,
    },
    Invoke {
        callee: Box<Expr>,
        args: Vec<CallArg>,
        block: Option<Block>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncParam {
    pub name: String,
    pub default: Option<Expr>,
}

/// An argument to a call, retaining its position in the source.
///
/// Keeping positional and keyword arguments in one sequence is important:
/// evaluation must follow source order even though parameter binding gives
/// positional arguments precedence.
#[derive(Debug, Clone, PartialEq)]
pub enum CallArg {
    Positional(Expr),
    Keyword { name: String, value: Expr },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expr(Expr),
    Assign {
        name: String,
        value: Expr,
    },
    Def {
        name: String,
        params: Vec<FuncParam>,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => ">=",
            Self::Le => "<=",
            Self::And => "&&",
            Self::Or => "||",
        };
        write!(f, "{s}")
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Not => "!",
            Self::Neg => "-",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPiece {
    Lit(String),
    Expr(Expr),
}
