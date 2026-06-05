//! Shared lexer for the four Nova Lingua surface sub-languages.
//!
//! A simple byte-walking tokenizer producing `(kind, text, byte-offset)`
//! triples, exactly as described in `spec/surface-syntax.md` §Lexer. Whitespace
//! and `--`-to-end-of-line comments are skipped between tokens.
//!
//! The token set is a superset of the spec's table: the spec's lexer table
//! omits the arithmetic / comparison / logic operators (`+ - * / % < > == != &&
//! || !`) even though its predicate and body grammars require them. That is an
//! editorial gap in the spec; the operators are supplied here so the lexer is
//! ready for all four sub-languages. The **type** sub-language uses only a
//! subset (identifiers, tags, content addresses, `-> . , : |` and brackets).

use std::fmt;

/// Token kinds produced by the lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokKind {
    /// `[a-z][a-z0-9_']*` — variable name, op name, field name.
    Ident,
    /// `[A-Z][A-Za-z0-9_]*` — variant tag, type constructor.
    Tag,
    /// `(fn|expr|type|proof|msg)_[0-9a-f]{64}` — content address.
    ContentAddr,
    /// `-?[0-9]+` integer literal (lexeme kept in `text`).
    Int,
    /// `-?[0-9]+\.[0-9]+` float literal (lexeme kept in `text`).
    Float,
    /// `"…"` string literal (decoded contents kept in `text`).
    Str,
    /// `b"[0-9a-fA-F]*"` hex bytes literal (hex digits kept in `text`).
    Bytes,
    Arrow,      // ->
    FatArrow,   // =>
    Dot,        // .
    Comma,      // ,
    Colon,      // :
    Eq,         // =
    Backslash,  // \
    Pipe,       // |
    Lparen,     // (
    Rparen,     // )
    Lbrace,     // {
    Rbrace,     // }
    Lbracket,   // [
    Rbracket,   // ]
    Semi,       // ;
    Underscore, // _
    // Operators required by the predicate / body grammars (see module note).
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    Lt,       // <
    Le,       // <=
    Gt,       // >
    Ge,       // >=
    EqEq,     // ==
    BangEq,   // !=
    AmpAmp,   // &&
    PipePipe, // ||
    Bang,     // !
    /// End of input sentinel; always the final token.
    Eof,
}

/// A single lexed token: its kind, its lexeme/decoded text, and the byte offset
/// at which it starts in the source string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    pub text: String,
    pub offset: usize,
}

/// A surface parse / lex error. Carries an optional byte offset so messages can
/// read `parse error at byte 12: expected '->' or end of type, found ','`, per
/// `spec/surface-syntax.md` §Implementation notes.
#[derive(Debug, Clone)]
pub struct SurfaceError {
    pub offset: Option<usize>,
    pub message: String,
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(o) => write!(f, "parse error at byte {o}: {}", self.message),
            None => write!(f, "parse error: {}", self.message),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl SurfaceError {
    /// An error anchored at a byte offset.
    pub fn at(offset: usize, message: impl Into<String>) -> Self {
        SurfaceError {
            offset: Some(offset),
            message: message.into(),
        }
    }

    /// An error with no specific offset (e.g. a malformed AST passed to unparse).
    pub fn msg(message: impl Into<String>) -> Self {
        SurfaceError {
            offset: None,
            message: message.into(),
        }
    }
}

/// True iff `s` matches the content-address grammar
/// `(fn|expr|type|proof|msg)_[0-9a-f]{64}`.
fn is_content_addr(s: &str) -> bool {
    let Some((prefix, rest)) = s.split_once('_') else {
        return false;
    };
    matches!(prefix, "fn" | "expr" | "type" | "proof" | "msg")
        && rest.len() == 64
        && rest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn one(kind: TokKind, text: &str, offset: usize) -> Token {
    Token {
        kind,
        text: text.to_string(),
        offset,
    }
}

/// Tokenize a surface string into a vector ending in [`TokKind::Eof`].
///
/// Whitespace and `--` line comments are skipped. Returns a [`SurfaceError`]
/// with a byte offset on the first malformed token.
pub fn tokenize(src: &str) -> Result<Vec<Token>, SurfaceError> {
    let b = src.as_bytes();
    let len = b.len();
    let mut pos = 0usize;
    let mut out: Vec<Token> = Vec::new();

    while pos < len {
        let c = b[pos];

        // Whitespace.
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            pos += 1;
            continue;
        }

        // `--` line comment to end of line.
        if c == b'-' && pos + 1 < len && b[pos + 1] == b'-' {
            pos += 2;
            while pos < len && b[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }

        let start = pos;

        // Multi-character operators.
        if c == b'-' && pos + 1 < len && b[pos + 1] == b'>' {
            out.push(one(TokKind::Arrow, "->", start));
            pos += 2;
            continue;
        }
        if c == b'=' && pos + 1 < len && b[pos + 1] == b'>' {
            out.push(one(TokKind::FatArrow, "=>", start));
            pos += 2;
            continue;
        }
        if c == b'=' && pos + 1 < len && b[pos + 1] == b'=' {
            out.push(one(TokKind::EqEq, "==", start));
            pos += 2;
            continue;
        }
        if c == b'!' && pos + 1 < len && b[pos + 1] == b'=' {
            out.push(one(TokKind::BangEq, "!=", start));
            pos += 2;
            continue;
        }
        if c == b'<' && pos + 1 < len && b[pos + 1] == b'=' {
            out.push(one(TokKind::Le, "<=", start));
            pos += 2;
            continue;
        }
        if c == b'>' && pos + 1 < len && b[pos + 1] == b'=' {
            out.push(one(TokKind::Ge, ">=", start));
            pos += 2;
            continue;
        }
        if c == b'&' && pos + 1 < len && b[pos + 1] == b'&' {
            out.push(one(TokKind::AmpAmp, "&&", start));
            pos += 2;
            continue;
        }
        if c == b'|' && pos + 1 < len && b[pos + 1] == b'|' {
            out.push(one(TokKind::PipePipe, "||", start));
            pos += 2;
            continue;
        }

        // Bytes literal `b"<hex>"`. `b` is also a valid ident start, so only
        // treat it as a bytes literal when an opening quote immediately follows.
        if c == b'b' && pos + 1 < len && b[pos + 1] == b'"' {
            let hstart = pos + 2;
            let mut p = hstart;
            while p < len && b[p] != b'"' {
                p += 1;
            }
            if p >= len {
                return Err(SurfaceError::at(start, "unterminated bytes literal"));
            }
            let hex = &src[hstart..p];
            if !hex.len().is_multiple_of(2) || !hex.bytes().all(|x| x.is_ascii_hexdigit()) {
                return Err(SurfaceError::at(
                    start,
                    "bytes literal must contain an even number of hex digits",
                ));
            }
            out.push(Token {
                kind: TokKind::Bytes,
                text: hex.to_string(),
                offset: start,
            });
            pos = p + 1;
            continue;
        }

        // String literal with `\"`, `\\`, `\n`, `\t` escapes.
        if c == b'"' {
            let mut p = pos + 1;
            let mut buf: Vec<u8> = Vec::new();
            loop {
                if p >= len {
                    return Err(SurfaceError::at(start, "unterminated string literal"));
                }
                let ch = b[p];
                if ch == b'\\' {
                    if p + 1 >= len {
                        return Err(SurfaceError::at(p, "dangling escape in string literal"));
                    }
                    match b[p + 1] {
                        b'"' => buf.push(b'"'),
                        b'\\' => buf.push(b'\\'),
                        b'n' => buf.push(b'\n'),
                        b't' => buf.push(b'\t'),
                        other => {
                            return Err(SurfaceError::at(
                                p,
                                format!("unsupported escape `\\{}`", other as char),
                            ))
                        }
                    }
                    p += 2;
                    continue;
                }
                if ch == b'"' {
                    p += 1;
                    break;
                }
                buf.push(ch);
                p += 1;
            }
            let text = String::from_utf8(buf)
                .map_err(|_| SurfaceError::at(start, "invalid UTF-8 in string literal"))?;
            out.push(Token {
                kind: TokKind::Str,
                text,
                offset: start,
            });
            pos = p;
            continue;
        }

        // Numbers: `-?[0-9]+` and `-?[0-9]+\.[0-9]+`. A leading `-` is part of
        // the literal only when a digit immediately follows; otherwise `-` is a
        // Minus operator (handled below).
        if c.is_ascii_digit() || (c == b'-' && pos + 1 < len && b[pos + 1].is_ascii_digit()) {
            let mut p = pos + 1;
            while p < len && b[p].is_ascii_digit() {
                p += 1;
            }
            let mut is_float = false;
            if p + 1 < len && b[p] == b'.' && b[p + 1].is_ascii_digit() {
                is_float = true;
                p += 1;
                while p < len && b[p].is_ascii_digit() {
                    p += 1;
                }
            }
            out.push(Token {
                kind: if is_float {
                    TokKind::Float
                } else {
                    TokKind::Int
                },
                text: src[pos..p].to_string(),
                offset: start,
            });
            pos = p;
            continue;
        }

        // Identifiers (lowercase-initial) and content addresses. Content
        // addresses lex as an identifier run first (all their characters are in
        // the ident-continue set) and are then reclassified.
        if c.is_ascii_lowercase() {
            let mut p = pos + 1;
            while p < len {
                let x = b[p];
                if x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'_' || x == b'\'' {
                    p += 1;
                } else {
                    break;
                }
            }
            let text = &src[pos..p];
            let kind = if is_content_addr(text) {
                TokKind::ContentAddr
            } else {
                TokKind::Ident
            };
            out.push(one(kind, text, start));
            pos = p;
            continue;
        }

        // Tags (uppercase-initial).
        if c.is_ascii_uppercase() {
            let mut p = pos + 1;
            while p < len {
                let x = b[p];
                if x.is_ascii_alphanumeric() || x == b'_' {
                    p += 1;
                } else {
                    break;
                }
            }
            out.push(one(TokKind::Tag, &src[pos..p], start));
            pos = p;
            continue;
        }

        // Single-character tokens.
        let (kind, txt) = match c {
            b'.' => (TokKind::Dot, "."),
            b',' => (TokKind::Comma, ","),
            b':' => (TokKind::Colon, ":"),
            b'=' => (TokKind::Eq, "="),
            b'\\' => (TokKind::Backslash, "\\"),
            b'|' => (TokKind::Pipe, "|"),
            b'(' => (TokKind::Lparen, "("),
            b')' => (TokKind::Rparen, ")"),
            b'{' => (TokKind::Lbrace, "{"),
            b'}' => (TokKind::Rbrace, "}"),
            b'[' => (TokKind::Lbracket, "["),
            b']' => (TokKind::Rbracket, "]"),
            b';' => (TokKind::Semi, ";"),
            b'_' => (TokKind::Underscore, "_"),
            b'+' => (TokKind::Plus, "+"),
            b'-' => (TokKind::Minus, "-"),
            b'*' => (TokKind::Star, "*"),
            b'/' => (TokKind::Slash, "/"),
            b'%' => (TokKind::Percent, "%"),
            b'<' => (TokKind::Lt, "<"),
            b'>' => (TokKind::Gt, ">"),
            b'!' => (TokKind::Bang, "!"),
            other => {
                return Err(SurfaceError::at(
                    start,
                    format!("unexpected character `{}`", other as char),
                ))
            }
        };
        out.push(one(kind, txt, start));
        pos += 1;
    }

    out.push(Token {
        kind: TokKind::Eof,
        text: String::new(),
        offset: len,
    });
    Ok(out)
}

/// A short human description of a token, for error messages.
pub fn describe(t: &Token) -> String {
    match t.kind {
        TokKind::Eof => "end of input".to_string(),
        TokKind::Ident => format!("identifier `{}`", t.text),
        TokKind::Tag => format!("tag `{}`", t.text),
        TokKind::ContentAddr => format!("content address `{}`", t.text),
        TokKind::Int => format!("integer `{}`", t.text),
        TokKind::Float => format!("float `{}`", t.text),
        TokKind::Str => "string literal".to_string(),
        TokKind::Bytes => "bytes literal".to_string(),
        _ => format!("`{}`", t.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_type_punctuation() {
        assert_eq!(
            kinds("forall a. (a -> b)"),
            vec![
                TokKind::Ident,
                TokKind::Ident,
                TokKind::Dot,
                TokKind::Lparen,
                TokKind::Ident,
                TokKind::Arrow,
                TokKind::Ident,
                TokKind::Rparen,
                TokKind::Eof,
            ]
        );
    }

    #[test]
    fn content_address_reclassified_from_ident_run() {
        let hex = "a".repeat(64);
        let toks = tokenize(&format!("type_{hex}")).unwrap();
        assert_eq!(toks[0].kind, TokKind::ContentAddr);
        assert_eq!(toks[0].text, format!("type_{hex}"));

        // Wrong length / non-hex stays an identifier.
        assert_eq!(tokenize("type_abc").unwrap()[0].kind, TokKind::Ident);
    }

    #[test]
    fn comments_and_whitespace_skipped() {
        assert_eq!(
            kinds("a -- a comment to EOL\n -> b"),
            vec![TokKind::Ident, TokKind::Arrow, TokKind::Ident, TokKind::Eof]
        );
    }

    #[test]
    fn arrow_is_not_a_comment() {
        assert_eq!(kinds("->"), vec![TokKind::Arrow, TokKind::Eof]);
    }

    #[test]
    fn numbers_signs_and_floats() {
        let toks = tokenize("42 -7 3.14 -0.5").unwrap();
        assert_eq!(toks[0].kind, TokKind::Int);
        assert_eq!(toks[0].text, "42");
        assert_eq!(toks[1].kind, TokKind::Int);
        assert_eq!(toks[1].text, "-7");
        assert_eq!(toks[2].kind, TokKind::Float);
        assert_eq!(toks[2].text, "3.14");
        assert_eq!(toks[3].kind, TokKind::Float);
        assert_eq!(toks[3].text, "-0.5");
    }

    #[test]
    fn minus_without_digit_is_operator() {
        assert_eq!(
            kinds("a - b"),
            vec![TokKind::Ident, TokKind::Minus, TokKind::Ident, TokKind::Eof]
        );
    }

    #[test]
    fn string_and_bytes_literals() {
        let toks = tokenize(r#""he\"llo" b"deadBEEF""#).unwrap();
        assert_eq!(toks[0].kind, TokKind::Str);
        assert_eq!(toks[0].text, "he\"llo");
        assert_eq!(toks[1].kind, TokKind::Bytes);
        assert_eq!(toks[1].text, "deadBEEF");
    }

    #[test]
    fn comparison_and_logic_operators() {
        assert_eq!(
            kinds("== != <= >= && || ! < >"),
            vec![
                TokKind::EqEq,
                TokKind::BangEq,
                TokKind::Le,
                TokKind::Ge,
                TokKind::AmpAmp,
                TokKind::PipePipe,
                TokKind::Bang,
                TokKind::Lt,
                TokKind::Gt,
                TokKind::Eof,
            ]
        );
    }

    #[test]
    fn unterminated_string_reports_offset() {
        let e = tokenize(r#"  "abc"#).unwrap_err();
        assert_eq!(e.offset, Some(2));
    }
}
