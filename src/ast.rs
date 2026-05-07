use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Str(String),
    Bool(bool),
    Nil,
    Var(String),
    Binary {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    Call {
        receiver: Option<Box<Expr>>,
        name: String,
        args: Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
        block: Option<Vec<Stmt>>,
    },
    Map(Vec<(String, Expr)>),
    Array(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
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
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Lt => "<",
            Self::Ge => ">=",
            Self::Le => "<=",
        };
        write!(f, "{s}")
    }
}
