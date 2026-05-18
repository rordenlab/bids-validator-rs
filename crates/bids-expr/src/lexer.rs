//! Tiny hand-written lexer.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Number(f64),
    String(String),
    Ident(String),

    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,

    Bang,
    EqEq,
    BangEq,
    AmpAmp,
    PipePipe,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,

    /// `in` keyword.
    In,
    /// `true` / `false` / `null` keywords (kept distinct so parser doesn't
    /// have to special-case identifiers).
    True,
    False,
    Null,

    Eof,
}

#[derive(Debug)]
pub struct LexError(pub String);

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lex: {}", self.0)
    }
}

pub fn lex(src: &str) -> Result<Vec<Tok>, LexError> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'.' if i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_digit() => {
                out.push(Tok::Dot);
                i += 1;
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::BangEq);
                    i += 2;
                } else {
                    out.push(Tok::Bang);
                    i += 1;
                }
            }
            b'=' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::EqEq);
                    i += 2;
                } else {
                    return Err(LexError("bare `=` not supported".into()));
                }
            }
            b'&' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    out.push(Tok::AmpAmp);
                    i += 2;
                } else {
                    return Err(LexError("bare `&` not supported".into()));
                }
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    out.push(Tok::PipePipe);
                    i += 2;
                } else {
                    return Err(LexError("bare `|` not supported".into()));
                }
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            b'*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    out.push(Tok::StarStar);
                    i += 2;
                } else {
                    out.push(Tok::Star);
                    i += 1;
                }
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        let nxt = bytes[i + 1];
                        match nxt {
                            b'n' => s.push('\n'),
                            b't' => s.push('\t'),
                            b'r' => s.push('\r'),
                            b'\\' => s.push('\\'),
                            b'\'' => s.push('\''),
                            b'"' => s.push('"'),
                            // Unknown escapes are preserved verbatim
                            // (matching JS-style string literals embedding
                            // regex patterns: `'\S'` → `\S` so the regex
                            // engine sees `\S` for non-whitespace).
                            other => {
                                s.push('\\');
                                s.push(other as char);
                            }
                        }
                        i += 2;
                    } else if bytes[i] < 0x80 {
                        s.push(bytes[i] as char);
                        i += 1;
                    } else {
                        // Multi-byte UTF-8 codepoint. Pre-fix this
                        // branch did `bytes[i] as char`, treating each
                        // raw byte as Latin-1 — non-ASCII string
                        // literals like "ééééé" parsed into garbage.
                        // `src` is `&str` so the bytes are guaranteed
                        // valid UTF-8; we can decode one char and
                        // advance by its UTF-8 length.
                        let rest = std::str::from_utf8(&bytes[i..])
                            .expect("input was &str, so suffix is valid UTF-8");
                        let ch = rest
                            .chars()
                            .next()
                            .expect("non-empty suffix yields at least one char");
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if i >= bytes.len() {
                    return Err(LexError("unterminated string".into()));
                }
                i += 1; // closing quote
                out.push(Tok::String(s));
            }
            c if c.is_ascii_digit()
                || (c == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let s =
                    std::str::from_utf8(&bytes[start..i]).map_err(|e| LexError(e.to_string()))?;
                let n: f64 = s
                    .parse()
                    .map_err(|e: std::num::ParseFloatError| LexError(e.to_string()))?;
                out.push(Tok::Number(n));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let s =
                    std::str::from_utf8(&bytes[start..i]).map_err(|e| LexError(e.to_string()))?;
                out.push(match s {
                    "in" => Tok::In,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "null" => Tok::Null,
                    _ => Tok::Ident(s.to_string()),
                });
            }
            _ => return Err(LexError(format!("unexpected byte 0x{c:02x} at offset {i}"))),
        }
    }

    out.push(Tok::Eof);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_tokens() {
        let toks = lex(r#"!("X" in sidecar)"#).unwrap();
        assert_eq!(toks[0], Tok::Bang);
        assert_eq!(toks[1], Tok::LParen);
        assert_eq!(toks[2], Tok::String("X".into()));
        assert_eq!(toks[3], Tok::In);
        assert_eq!(toks[4], Tok::Ident("sidecar".into()));
        assert_eq!(toks[5], Tok::RParen);
        assert_eq!(toks[6], Tok::Eof);
    }

    #[test]
    fn equality_and_call() {
        let toks = lex(r#"match(extension, "^\\.nii(\\.gz)?$")"#).unwrap();
        assert_eq!(toks[0], Tok::Ident("match".into()));
        assert_eq!(toks[1], Tok::LParen);
        assert_eq!(toks[2], Tok::Ident("extension".into()));
        assert_eq!(toks[3], Tok::Comma);
        // Don't check the exact regex string contents (escape rules) — just type
        assert!(matches!(toks[4], Tok::String(_)));
        assert_eq!(toks[5], Tok::RParen);
    }
}
