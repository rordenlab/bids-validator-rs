//! Pratt-style recursive-descent parser for the selector subset.

use crate::ast::{BinaryOp, Expr, UnaryOp};
use crate::lexer::{lex, LexError, Tok};

#[derive(Debug)]
pub enum ParseError {
    Lex(LexError),
    Unexpected(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lex(e) => write!(f, "{e}"),
            Self::Unexpected(s) => write!(f, "parse: {s}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let toks = lex(src).map_err(ParseError::Lex)?;
    let mut p = Parser { toks, i: 0 };
    let e = p.parse_or()?;
    if p.peek() != &Tok::Eof {
        return Err(ParseError::Unexpected(format!(
            "trailing tokens: {:?}",
            &p.toks[p.i..]
        )));
    }
    Ok(e)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.i]
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.i].clone();
        self.i += 1;
        t
    }

    fn expect(&mut self, t: &Tok) -> Result<(), ParseError> {
        if self.peek() == t {
            self.bump();
            Ok(())
        } else {
            Err(ParseError::Unexpected(format!(
                "expected {:?}, got {:?}",
                t,
                self.peek()
            )))
        }
    }

    // Pratt grammar (lowest precedence first):
    //   or       := and ("||" and)*
    //   and      := equality ("&&" equality)*
    //   equality := comparison (("==" | "!=") comparison)*
    //   comparison := in_op (("<" | "<=" | ">" | ">=") in_op)*
    //   in_op    := unary ("in" unary)?            // right-associative not needed
    //   unary    := ("!" | "-") unary | postfix
    //   postfix  := primary ("." Ident | "(" args ")")*
    //   primary  := literal | "(" or ")" | Ident | "[" args "]"

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.peek() == &Tok::PipePipe {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::Binary(BinaryOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_equality()?;
        while self.peek() == &Tok::AmpAmp {
            self.bump();
            let rhs = self.parse_equality()?;
            lhs = Expr::Binary(BinaryOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinaryOp::Eq,
                Tok::BangEq => BinaryOp::Neq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_comparison()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_in()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinaryOp::Lt,
                Tok::Le => BinaryOp::Le,
                Tok::Gt => BinaryOp::Gt,
                Tok::Ge => BinaryOp::Ge,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_in()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_in(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_term()?;
        if self.peek() == &Tok::In {
            self.bump();
            let rhs = self.parse_term()?;
            Ok(Expr::In(Box::new(lhs), Box::new(rhs)))
        } else {
            Ok(lhs)
        }
    }

    fn parse_term(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_factor()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinaryOp::Add,
                Tok::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_factor()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_factor(&mut self) -> Result<Expr, ParseError> {
        // `*`, `/`, `%` bind tighter than `+`/`-` but looser than `**`.
        let mut lhs = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinaryOp::Mul,
                Tok::Slash => BinaryOp::Div,
                Tok::Percent => BinaryOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_power()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// `**` is right-associative and binds tighter than `*`/`/`/`%`.
    /// Mirrors JS exponentiation precedence (per ECMA-262).
    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_unary()?;
        if self.peek() == &Tok::StarStar {
            self.bump();
            let rhs = self.parse_power()?; // right-assoc
            Ok(Expr::Binary(BinaryOp::Pow, Box::new(lhs), Box::new(rhs)))
        } else {
            Ok(lhs)
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Tok::Bang => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary(UnaryOp::Not, Box::new(inner)))
            }
            Tok::Minus => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary(UnaryOp::Neg, Box::new(inner)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let name = match self.bump() {
                        Tok::Ident(s) => s,
                        other => {
                            return Err(ParseError::Unexpected(format!(
                                "expected identifier after `.`, got {other:?}"
                            )));
                        }
                    };
                    e = Expr::Member(Box::new(e), name);
                }
                Tok::LParen => {
                    // Only allowed when `e` is a bare identifier — selectors don't
                    // do `expr(args)`. Reject anything else.
                    let name = match e {
                        Expr::Ident(s) => s,
                        _ => {
                            return Err(ParseError::Unexpected(
                                "call target must be a bare identifier".into(),
                            ))
                        }
                    };
                    self.bump();
                    let args = self.parse_args(&Tok::RParen)?;
                    e = Expr::Call(name, args);
                }
                Tok::LBracket => {
                    self.bump();
                    let idx = self.parse_or()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_args(&mut self, end: &Tok) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == end {
            self.bump();
            return Ok(args);
        }
        loop {
            args.push(self.parse_or()?);
            match self.peek() {
                Tok::Comma => {
                    self.bump();
                }
                t if t == end => {
                    self.bump();
                    break;
                }
                other => {
                    return Err(ParseError::Unexpected(format!(
                        "in argument list, expected `,` or `{end:?}`, got {other:?}"
                    )));
                }
            }
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.bump() {
            Tok::Number(n) => Ok(Expr::Number(n)),
            Tok::String(s) => Ok(Expr::String(s)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Null => Ok(Expr::Null),
            Tok::Ident(s) => Ok(Expr::Ident(s)),
            Tok::LParen => {
                let e = self.parse_or()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                let elements = self.parse_args(&Tok::RBracket)?;
                Ok(Expr::Array(elements))
            }
            other => Err(ParseError::Unexpected(format!("primary token: {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) {
        parse(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
    }

    #[test]
    fn parses_common_selectors() {
        p(r#"modality == "mri""#);
        p(r#"match(extension, "^\\.nii(\\.gz)?$")"#);
        p(r#"!("IntendedFor" in sidecar)"#);
        p(r#"intersects([suffix], ["dseg", "probseg", "mask"])"#);
        p(r#"!intersects([suffix], ["events", "beh"])"#);
        p(r#"datatype == "anat" && suffix == "T1w""#);
        p(r#""ce" in entities"#);
        p(r#""micr" in dataset.modalities"#);
        p(r#"!exists("CITATION.cff", "dataset")"#);
        p(r#"sidecar.RepetitionTime == 2.0"#);
    }
}
