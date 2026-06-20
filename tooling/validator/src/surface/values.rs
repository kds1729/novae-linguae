//! Value-expression surface syntax: parser (string -> AST) and pretty-printer
//! (AST -> canonical string).
//!
//! Grammar (from `spec/surface-syntax.md` §3, targeting
//! `value-expression.schema.json`):
//!
//! ```text
//! value ::= "true" | "false"                       -- bool
//!         | int_literal                            -- nat (>= 0) or int (< 0)
//!         | "int" "(" int_literal ")"              -- explicit int (non-negative)
//!         | float_literal                          -- float
//!         | str_literal                            -- string
//!         | bytes_literal                          -- bytes (b"<hex>")
//!         | "(" ")"                                -- unit
//!         | "[" (value ("," value)*)? "]"          -- list
//!         | "(" value ("," value)+ ")"             -- tuple (2+)
//!         | "(" value ")"                          -- grouping (lenient; never emitted)
//!         | "{" (field ("," field)*)? "}"          -- record, field = ident "=" value
//!         | tag ("(" value ")")?                   -- variant
//!         | content_addr                           -- fn_ref (fn_… only)
//! ```
//!
//! ## Disambiguation rules (from the spec)
//!
//! * A non-negative integer literal parses to `nat`; a negative one (`-N`) to
//!   `int`. To force `int` for a non-negative value, use `int(N)`. Accordingly
//!   the pretty-printer emits a non-negative `int` as `int(N)` and a negative
//!   one as `-N`, so the kind survives the round trip.
//! * `bytes` are `b"<hex>"` on the surface but base64 in the AST
//!   (per the schema); the parser hex-decodes then base64-encodes, and the
//!   printer reverses that, emitting lowercase hex.

use base64::Engine;
use serde_json::{json, Value};

use super::lexer::{describe, tokenize, SurfaceError, TokKind, Token};

// ---- parser ----

/// Parse a value-expression surface string into its JSON AST (conforming to
/// `spec/value-expression.schema.json`).
pub fn parse_value(src: &str) -> Result<Value, SurfaceError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, i: 0 };
    let v = p.parse_value()?;
    if !p.at(&TokKind::Eof) {
        let t = p.peek();
        return Err(SurfaceError::at(
            t.offset,
            format!("unexpected trailing input: {}", describe(t)),
        ));
    }
    Ok(v)
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.toks[self.i]
    }

    fn kind(&self) -> &TokKind {
        &self.toks[self.i].kind
    }

    fn at(&self, k: &TokKind) -> bool {
        self.kind() == k
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        t
    }

    fn expect(&mut self, k: TokKind, what: &str) -> Result<Token, SurfaceError> {
        if self.kind() == &k {
            Ok(self.bump())
        } else {
            let t = self.peek();
            Err(SurfaceError::at(
                t.offset,
                format!("expected {what}, found {}", describe(t)),
            ))
        }
    }

    fn parse_value(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Ident => {
                let t = self.bump();
                match t.text.as_str() {
                    "true" => Ok(json!({ "kind": "bool", "value": true })),
                    "false" => Ok(json!({ "kind": "bool", "value": false })),
                    "int" => {
                        self.expect(TokKind::Lparen, "`(` after `int`")?;
                        let n = self.expect(TokKind::Int, "an integer literal inside `int(...)`")?;
                        self.expect(TokKind::Rparen, "`)` to close `int(...)`")?;
                        Ok(json!({ "kind": "int", "value": int_from_text(&n.text) }))
                    }
                    other => Err(SurfaceError::at(
                        t.offset,
                        format!(
                            "`{other}` is not a value (bare identifiers are not values; did you mean a `string`, a `bool`, or `int(...)`?)"
                        ),
                    )),
                }
            }
            TokKind::Int => {
                let t = self.bump();
                if t.text.starts_with('-') {
                    Ok(json!({ "kind": "int", "value": int_from_text(&t.text) }))
                } else {
                    Ok(json!({ "kind": "nat", "value": nat_from_text(&t.text) }))
                }
            }
            TokKind::Float => {
                let t = self.bump();
                Ok(json!({ "kind": "float", "value": float_from_text(&t.text, t.offset)? }))
            }
            TokKind::Str => {
                let t = self.bump();
                Ok(json!({ "kind": "string", "value": t.text }))
            }
            TokKind::Bytes => {
                let t = self.bump();
                let raw = hex_to_bytes(&t.text, t.offset)?;
                let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
                Ok(json!({ "kind": "bytes", "value": b64 }))
            }
            TokKind::ContentAddr => {
                let t = self.bump();
                if t.text.starts_with("fn_") {
                    Ok(json!({ "kind": "fn_ref", "target": t.text }))
                } else {
                    Err(SurfaceError::at(
                        t.offset,
                        format!(
                            "only `fn_…` content addresses are valid in a value position, got `{}`",
                            t.text
                        ),
                    ))
                }
            }
            TokKind::Tag => {
                let t = self.bump();
                let mut variant = serde_json::Map::new();
                variant.insert("kind".to_string(), Value::String("variant".to_string()));
                variant.insert("tag".to_string(), Value::String(t.text));
                if self.at(&TokKind::Lparen) {
                    self.bump();
                    let payload = self.parse_value()?;
                    self.expect(TokKind::Rparen, "`)` after variant payload")?;
                    variant.insert("payload".to_string(), payload);
                }
                Ok(Value::Object(variant))
            }
            TokKind::Lparen => {
                self.bump();
                if self.at(&TokKind::Rparen) {
                    self.bump();
                    return Ok(json!({ "kind": "unit" }));
                }
                let first = self.parse_value()?;
                if self.at(&TokKind::Comma) {
                    let mut elems = vec![first];
                    while self.at(&TokKind::Comma) {
                        self.bump();
                        elems.push(self.parse_value()?);
                    }
                    self.expect(TokKind::Rparen, "`)` to close tuple")?;
                    Ok(json!({ "kind": "tuple", "elems": elems }))
                } else {
                    // Lenient grouping; the canonical printer never emits this.
                    self.expect(TokKind::Rparen, "`)` to close parenthesized value")?;
                    Ok(first)
                }
            }
            TokKind::Lbracket => {
                self.bump();
                let mut elems: Vec<Value> = Vec::new();
                if !self.at(&TokKind::Rbracket) {
                    loop {
                        elems.push(self.parse_value()?);
                        if self.at(&TokKind::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(TokKind::Rbracket, "`]` to close list")?;
                Ok(json!({ "kind": "list", "elems": elems }))
            }
            TokKind::Lbrace => {
                self.bump();
                let mut fields: Vec<Value> = Vec::new();
                if !self.at(&TokKind::Rbrace) {
                    loop {
                        let name = self.expect(TokKind::Ident, "a field name")?;
                        self.expect(TokKind::Eq, "`=` after field name")?;
                        let val = self.parse_value()?;
                        fields.push(json!({ "name": name.text, "value": val }));
                        if self.at(&TokKind::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(TokKind::Rbrace, "`}` to close record")?;
                Ok(json!({ "kind": "record", "fields": fields }))
            }
            _ => {
                let t = self.peek();
                Err(SurfaceError::at(
                    t.offset,
                    format!("expected a value, found {}", describe(t)),
                ))
            }
        }
    }
}

/// Build a `nat` value from a non-negative digit string. Small values become
/// JSON numbers; values beyond `u64` become canonical decimal strings (per the
/// schema's big-int convention).
fn nat_from_text(digits: &str) -> Value {
    let trimmed = digits.trim_start_matches('0');
    let t = if trimmed.is_empty() { "0" } else { trimmed };
    match t.parse::<u64>() {
        Ok(u) => json!(u),
        Err(_) => Value::String(t.to_string()),
    }
}

/// Build an `int` value from a (possibly signed) integer lexeme. `pub(crate)` so the body parser can
/// reuse it for the `int(N)` typed-literal form (mirroring this module's value-syntax handling).
pub(crate) fn int_from_text(s: &str) -> Value {
    let (neg, ds) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let trimmed = ds.trim_start_matches('0');
    let t = if trimmed.is_empty() { "0" } else { trimmed };
    if t == "0" {
        return json!(0);
    }
    if neg {
        let signed = format!("-{t}");
        match signed.parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => Value::String(signed),
        }
    } else {
        match t.parse::<i64>() {
            Ok(i) => json!(i),
            Err(_) => Value::String(t.to_string()),
        }
    }
}

/// Build a `float` value, rejecting non-finite inputs (JCS forbids them).
fn float_from_text(s: &str, offset: usize) -> Result<Value, SurfaceError> {
    let f: f64 = s
        .parse()
        .map_err(|_| SurfaceError::at(offset, format!("invalid float literal `{s}`")))?;
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .ok_or_else(|| SurfaceError::at(offset, "float literal is not finite"))
}

/// Decode an even-length hex string to raw bytes. The lexer already guarantees
/// even length and valid hex digits; the checks here are defensive.
fn hex_to_bytes(hex: &str, offset: usize) -> Result<Vec<u8>, SurfaceError> {
    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(SurfaceError::at(offset, "hex string must have even length"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_digit(pair[0], offset)?;
        let lo = hex_digit(pair[1], offset)?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_digit(b: u8, offset: usize) -> Result<u8, SurfaceError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => Err(SurfaceError::at(
            offset,
            format!("invalid hex digit `{}`", other as char),
        )),
    }
}

/// Build a value AST from a single scalar literal token (`Int`, `Float`, `Str`,
/// `Bytes`). Returns `None` for other token kinds. Shared with the body
/// sub-language, where literals embed value-expressions.
pub(crate) fn scalar_value_from_token(t: &Token) -> Option<Result<Value, SurfaceError>> {
    match t.kind {
        TokKind::Int => Some(Ok(if t.text.starts_with('-') {
            json!({ "kind": "int", "value": int_from_text(&t.text) })
        } else {
            json!({ "kind": "nat", "value": nat_from_text(&t.text) })
        })),
        TokKind::Float => {
            Some(float_from_text(&t.text, t.offset).map(|v| json!({ "kind": "float", "value": v })))
        }
        TokKind::Str => Some(Ok(json!({ "kind": "string", "value": t.text.clone() }))),
        TokKind::Bytes => Some(hex_to_bytes(&t.text, t.offset).map(|raw| {
            json!({
                "kind": "bytes",
                "value": base64::engine::general_purpose::STANDARD.encode(raw)
            })
        })),
        _ => None,
    }
}

// ---- pretty-printer (canonical) ----

fn node_kind(v: &Value) -> Option<&str> {
    v.get("kind").and_then(|k| k.as_str())
}

/// Canonical decimal string (with sign) for a `nat`/`int` `value` field, which
/// may be a JSON number or a big-int string.
fn integer_decimal(v: &Value) -> Result<String, SurfaceError> {
    if let Some(u) = v.as_u64() {
        Ok(u.to_string())
    } else if let Some(i) = v.as_i64() {
        Ok(i.to_string())
    } else if let Some(s) = v.as_str() {
        Ok(s.to_string())
    } else {
        Err(SurfaceError::msg(
            "integer value must be a number or decimal string",
        ))
    }
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").expect("writing to String is infallible");
    }
    s
}

/// Pretty-print a value-expression AST to its canonical surface string.
///
/// Canonical form per `spec/surface-syntax.md` §3: `float`s always carry a
/// decimal point; non-negative `int`s render as `int(N)` (negative as `-N`);
/// `bytes` render as lowercase hex; record fields are sorted by name.
pub fn unparse_value(ast: &Value) -> Result<String, SurfaceError> {
    let kind = node_kind(ast)
        .ok_or_else(|| SurfaceError::msg("value expression must be a JSON object with a `kind`"))?;

    match kind {
        "bool" => {
            let b = ast
                .get("value")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| SurfaceError::msg("`bool` value must be a boolean"))?;
            Ok(if b { "true" } else { "false" }.to_string())
        }
        "nat" => {
            let v = ast
                .get("value")
                .ok_or_else(|| SurfaceError::msg("`nat` missing `value`"))?;
            let s = integer_decimal(v)?;
            if s.starts_with('-') {
                return Err(SurfaceError::msg("`nat` value must be non-negative"));
            }
            Ok(s)
        }
        "int" => {
            let v = ast
                .get("value")
                .ok_or_else(|| SurfaceError::msg("`int` missing `value`"))?;
            let s = integer_decimal(v)?;
            if s.starts_with('-') {
                Ok(s)
            } else {
                // Force the int kind on the surface for non-negative magnitudes.
                Ok(format!("int({s})"))
            }
        }
        "float" => {
            let f = ast
                .get("value")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| SurfaceError::msg("`float` value must be a number"))?;
            let mut s = format!("{f}");
            if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                s.push_str(".0");
            }
            Ok(s)
        }
        "string" => {
            let s = ast
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`string` value must be a string"))?;
            Ok(escape_string(s))
        }
        "bytes" => {
            let b64 = ast
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`bytes` value must be a base64 string"))?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    SurfaceError::msg(format!("`bytes` value is not valid base64: {e}"))
                })?;
            Ok(format!("b\"{}\"", bytes_to_hex(&raw)))
        }
        "unit" => Ok("()".to_string()),
        "list" => {
            let elems = ast
                .get("elems")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`list` missing `elems`"))?;
            let parts = elems
                .iter()
                .map(unparse_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", parts.join(", ")))
        }
        "tuple" => {
            let elems = ast
                .get("elems")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`tuple` missing `elems`"))?;
            let parts = elems
                .iter()
                .map(unparse_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(", ")))
        }
        "record" => {
            let fields = ast
                .get("fields")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`record` missing `fields`"))?;
            let mut pairs: Vec<(String, String)> = Vec::with_capacity(fields.len());
            for f in fields {
                let name = f
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SurfaceError::msg("record field missing `name`"))?;
                let val = f
                    .get("value")
                    .ok_or_else(|| SurfaceError::msg("record field missing `value`"))?;
                pairs.push((name.to_string(), unparse_value(val)?));
            }
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let body = pairs
                .into_iter()
                .map(|(n, v)| format!("{n} = {v}"))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("{{{body}}}"))
        }
        "variant" => {
            let tag = ast
                .get("tag")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`variant` missing `tag`"))?;
            if let Some(payload) = ast.get("payload") {
                Ok(format!("{tag}({})", unparse_value(payload)?))
            } else {
                Ok(tag.to_string())
            }
        }
        "fn_ref" => {
            let target = ast
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`fn_ref` missing `target`"))?;
            Ok(target.to_string())
        }
        other => Err(SurfaceError::msg(format!(
            "unknown value-expression kind `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parses_to(src: &str, expected: Value) {
        assert_eq!(parse_value(src).unwrap(), expected, "parse of `{src}`");
    }

    #[test]
    fn parse_scalars() {
        parses_to("true", json!({"kind": "bool", "value": true}));
        parses_to("false", json!({"kind": "bool", "value": false}));
        parses_to("42", json!({"kind": "nat", "value": 42}));
        parses_to("-7", json!({"kind": "int", "value": -7}));
        parses_to("int(0)", json!({"kind": "int", "value": 0}));
        parses_to("int(5)", json!({"kind": "int", "value": 5}));
        // Avoid 3.14 here: clippy::approx_constant (deny-by-default) flags it.
        parses_to("3.25", json!({"kind": "float", "value": 3.25}));
        parses_to("\"hello\"", json!({"kind": "string", "value": "hello"}));
        parses_to("()", json!({"kind": "unit"}));
    }

    #[test]
    fn parse_bytes_hex_to_base64() {
        // b"deadbeef" -> base64 of [0xde,0xad,0xbe,0xef]
        parses_to(
            "b\"deadbeef\"",
            json!({"kind": "bytes", "value": "3q2+7w=="}),
        );
    }

    #[test]
    fn parse_compound() {
        parses_to(
            "[1, 2, 3]",
            json!({
                "kind": "list",
                "elems": [
                    {"kind": "nat", "value": 1},
                    {"kind": "nat", "value": 2},
                    {"kind": "nat", "value": 3}
                ]
            }),
        );
        parses_to(
            "(true, 42)",
            json!({
                "kind": "tuple",
                "elems": [
                    {"kind": "bool", "value": true},
                    {"kind": "nat", "value": 42}
                ]
            }),
        );
        parses_to(
            "{x = 1, y = 2}",
            json!({
                "kind": "record",
                "fields": [
                    {"name": "x", "value": {"kind": "nat", "value": 1}},
                    {"name": "y", "value": {"kind": "nat", "value": 2}}
                ]
            }),
        );
        parses_to(
            "Some(42)",
            json!({"kind": "variant", "tag": "Some", "payload": {"kind": "nat", "value": 42}}),
        );
        parses_to("None", json!({"kind": "variant", "tag": "None"}));
        let hex = "0".repeat(64);
        parses_to(
            &format!("fn_{hex}"),
            json!({"kind": "fn_ref", "target": format!("fn_{hex}")}),
        );
    }

    #[test]
    fn parse_errors() {
        assert!(parse_value("foo").is_err()); // bare ident is not a value
        assert!(parse_value("int").is_err()); // bare int without (...)
        assert!(parse_value("(1)").is_ok()); // lenient grouping
        assert!(parse_value("[1,").is_err()); // dangling list
        let hex = "0".repeat(64);
        assert!(parse_value(&format!("type_{hex}")).is_err()); // wrong content-addr kind
    }

    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "true",
            "false",
            "42",
            "-7",
            "int(0)",
            "int(5)",
            "3.14",
            "1.0",
            "\"hello\"",
            "\"he\\\"llo\"",
            "b\"deadbeef\"",
            "()",
            "[]",
            "[1, 2, 3]",
            "(true, 42)",
            "{x = 1, y = 2}",
            "Some(42)",
            "None",
        ] {
            let ast = parse_value(s).unwrap();
            let printed = unparse_value(&ast).unwrap();
            assert_eq!(printed, s, "unparse(parse({s:?})) should be canonical");
        }
    }

    #[test]
    fn canonical_asts_round_trip() {
        for ast in [
            json!({"kind": "nat", "value": 0}),
            json!({"kind": "int", "value": -7}),
            json!({"kind": "int", "value": 9}),
            json!({"kind": "float", "value": 2.5}),
            json!({"kind": "bytes", "value": "3q2+7w=="}),
            json!({
                "kind": "record",
                "fields": [
                    {"name": "a", "value": {"kind": "bool", "value": true}},
                    {"name": "b", "value": {"kind": "nat", "value": 1}}
                ]
            }),
        ] {
            let printed = unparse_value(&ast).unwrap();
            let reparsed = parse_value(&printed).unwrap();
            assert_eq!(reparsed, ast, "parse(unparse(ast)) should equal ast");
        }
    }

    #[test]
    fn unparse_sorts_record_fields() {
        let ast = parse_value("{y = 2, x = 1}").unwrap();
        assert_eq!(unparse_value(&ast).unwrap(), "{x = 1, y = 2}");
    }

    #[test]
    fn nonnegative_int_round_trips_as_explicit() {
        // int(5) must NOT collapse to a nat on the round trip.
        let ast = parse_value("int(5)").unwrap();
        assert_eq!(ast["kind"], "int");
        assert_eq!(unparse_value(&ast).unwrap(), "int(5)");
    }
}
