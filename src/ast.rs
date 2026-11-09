use crate::error::Span;

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        value: Expr,
        span: Span,
    },
    Fn(FnDecl),
    Expr(Expr),
    /// `target = value`, or `target op= value` when `op` is set.
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
        span: Span,
    },
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Block(Vec<Stmt>),
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Break(Span),
    Continue(Span),
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Nil(Span),
    Bool(bool, Span),
    Int(i64, Span),
    Float(f64, Span),
    Str(String, Span),
    /// An interpolated string: the parts are concatenated as text.
    Interp(Vec<Expr>, Span),
    Var(String, Span),
    List(Vec<Expr>, Span),
    Map(Vec<(String, Expr)>, Span),
    Neg(Box<Expr>, Span),
    Not(Box<Expr>, Span),
    Binary(BinOp, Box<Expr>, Box<Expr>, Span),
    And(Box<Expr>, Box<Expr>, Span),
    Or(Box<Expr>, Box<Expr>, Span),
    Call(Box<Expr>, Vec<Expr>, Span),
    Index(Box<Expr>, Box<Expr>, Span),
    Field(Box<Expr>, String, Span),
    Lambda(Box<FnDecl>),
    /// `await name(args)`: hands control to the host, which performs the effect and resumes the program.
    Await {
        effect: String,
        args: Vec<Expr>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Nil(s)
            | Expr::Bool(_, s)
            | Expr::Int(_, s)
            | Expr::Float(_, s)
            | Expr::Str(_, s)
            | Expr::Interp(_, s)
            | Expr::Var(_, s)
            | Expr::List(_, s)
            | Expr::Map(_, s)
            | Expr::Neg(_, s)
            | Expr::Not(_, s)
            | Expr::Binary(_, _, _, s)
            | Expr::And(_, _, s)
            | Expr::Or(_, _, s)
            | Expr::Call(_, _, s)
            | Expr::Index(_, _, s)
            | Expr::Field(_, _, s)
            | Expr::Await { span: s, .. } => *s,
            Expr::Lambda(f) => f.span,
        }
    }
}
