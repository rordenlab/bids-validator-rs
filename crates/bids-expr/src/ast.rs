//! Expression AST. Kept intentionally small.

#[derive(Debug, Clone)]
pub enum Expr {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Expr>),

    /// Bare identifier — looked up in the top-level context scope.
    Ident(String),

    /// Member access: `expr.name`. We model dotted access as a chain of
    /// `Member` nodes so `entities.task` parses as
    /// `Member(Ident("entities"), "task")`.
    Member(Box<Expr>, String),

    /// `f(args...)`. The callee is always a bare identifier in selectors;
    /// we keep this shape (rather than `Call(Expr, ...)`) until evidence
    /// shows otherwise.
    Call(String, Vec<Expr>),

    /// `lhs in rhs` — JS `in` operator (property-exists on object).
    In(Box<Expr>, Box<Expr>),

    /// `base[index]` — array / string element access. Index is evaluated;
    /// out-of-range yields `Undefined` (JS semantics).
    Index(Box<Expr>, Box<Expr>),

    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Eq,
    Neq,
    And,
    Or,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}
