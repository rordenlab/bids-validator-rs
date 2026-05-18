//! BIDS expression-language evaluator.
#![forbid(unsafe_code)]
//!
//! Implements the JS-flavored mini-DSL used in schema `selectors`, `checks`,
//! and `level_addendum` strings. The TS reference implementation
//! (`src/schema/expressionLanguage.ts`) literally wraps each expression in
//! `new Function('context', 'with(context) { return …}')`, so semantics here
//! are JavaScript:
//!
//!   * `==` is value-equal with coercion (we implement loose equality
//!     matching the cases that show up in schema expressions, not full
//!     ECMA-262 abstract equality).
//!   * Truthiness: `0`, `""`, `null`, `undefined`, and `NaN` are falsy.
//!     Empty arrays/objects are truthy (JS quirk).
//!   * `&&` / `||` are short-circuit and return the actual operand value.
//!   * `!` returns a boolean.
//!   * `"x" in obj` returns `true` if the property exists.
//!   * Bare identifiers look up in the context's top-level scope.
//!
//! Scope of v1: covers every operator and built-in function actually used
//! by `rules.sidecars.*.selectors` and the simpler `rules.checks` selectors.
//! Things like `+`/`-` numeric arithmetic, ternary `?:`, regex literals,
//! and object literals are not yet implemented; the parser will return
//! an error rather than silently accept them.

mod ast;
mod eval;
mod lexer;
mod parser;
mod value;

pub use ast::Expr;
pub use eval::{eval, eval_with, exists, EvalContext};
pub use parser::{parse, ParseError};
pub use value::Value;

/// Convenience: parse and evaluate a single expression against a context.
pub fn parse_and_eval(src: &str, ctx: &Value) -> Result<Value, EvalError> {
    let expr = parse(src).map_err(EvalError::Parse)?;
    eval(&expr, ctx)
}

#[derive(Debug)]
pub enum EvalError {
    Parse(ParseError),
    UnknownIdentifier(String),
    UnknownFunction(String),
    BadFunctionCall { name: String, reason: String },
    TypeError(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::UnknownIdentifier(s) => write!(f, "unknown identifier: {s}"),
            Self::UnknownFunction(s) => write!(f, "unknown function: {s}"),
            Self::BadFunctionCall { name, reason } => {
                write!(f, "bad call to {name}: {reason}")
            }
            Self::TypeError(s) => write!(f, "type error: {s}"),
        }
    }
}

impl std::error::Error for EvalError {}
