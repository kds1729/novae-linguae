//! Predicate-expression surface syntax: parser (string -> AST) and
//! pretty-printer (AST -> canonical string).
//!
//! Grammar (from `spec/surface-syntax.md` §2, targeting
//! `predicate-expression.schema.json`). Infix operators are sugar for `app`
//! nodes; precedence follows conventional arithmetic/logic rules:
//!
//! ```text
//! pred      ::= "forall" ident+ "." pred | "exists" ident+ "." pred | or_pred
//! or_pred   ::= and_pred ("||" and_pred)*          -- app {op:"or"}   left-assoc
//! and_pred  ::= eq_pred  ("&&" eq_pred)*           -- app {op:"and"}  left-assoc
//! eq_pred   ::= cmp_pred (("==" | "!=") cmp_pred)? -- app {op:"eq"/"neq"}  (non-assoc)
//! cmp_pred  ::= add_pred (("<"|"<="|">"|">=") add_pred)? -- lt/le/gt/ge  (non-assoc)
//! add_pred  ::= mul_pred (("+"|"-") mul_pred)*     -- add/sub  left-assoc
//! mul_pred  ::= unary    (("*"|"/"|"%") unary)*    -- mul/div/mod  left-assoc
//! unary     ::= "!" unary | "-" unary | call_pred  -- not / neg
//! call_pred ::= name ("(" (pred ("," pred)*)? ")")? | atom
//! atom      ::= ident | content_addr | int | float | str | "true" | "false" | "(" pred ")"
//! ```
//!
//! ## Where this diverges from `spec/surface-syntax.md`
//!
//! The spec's infix-mapping table renders `<=` as `lte` and `>=` as `gte`, but
//! the committed `predicate-expression.schema.json` op vocabulary (and the
//! validator's arity table) use **`le`/`ge`**. The schema wins, so `<=`→`le`
//! and `>=`→`ge` here. (Flagged for reconciliation in surface-syntax.md.)
//!
//! Note: the schema permits names with a leading underscore (`_x`), but the
//! shared lexer's identifier rule starts at `[a-z]`, so leading-underscore names
//! are not expressible on the surface in v0.1. The names that actually occur
//! (`input`, `output`, quantified vars) don't need it.

use serde_json::{json, Value};

use super::lexer::{describe, tokenize, SurfaceError, TokKind, Token};

// ---- parser ----

/// Parse a predicate-expression surface string into its JSON AST (conforming to
/// `spec/predicate-expression.schema.json`).
pub fn parse_predicate(src: &str) -> Result<Value, SurfaceError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, i: 0 };
    let v = p.parse_pred()?;
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

fn binop(op: &str, l: Value, r: Value) -> Value {
    json!({ "kind": "app", "op": op, "args": [l, r] })
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

    fn at_keyword(&self, name: &str) -> bool {
        matches!(self.kind(), TokKind::Ident) && self.peek().text == name
    }

    fn parse_pred(&mut self) -> Result<Value, SurfaceError> {
        if self.at_keyword("forall") || self.at_keyword("exists") {
            let kw = self.bump().text; // "forall" | "exists"
            let mut vars: Vec<String> = Vec::new();
            while matches!(self.kind(), TokKind::Ident) {
                vars.push(self.bump().text);
            }
            if vars.is_empty() {
                let t = self.peek();
                return Err(SurfaceError::at(
                    t.offset,
                    format!(
                        "expected at least one variable after `{kw}`, found {}",
                        describe(t)
                    ),
                ));
            }
            self.expect(TokKind::Dot, "`.` after quantifier binders")?;
            let body = self.parse_pred()?;
            return Ok(json!({ "kind": kw, "vars": vars, "body": body }));
        }
        self.or_pred()
    }

    fn or_pred(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.and_pred()?;
        while self.at(&TokKind::PipePipe) {
            self.bump();
            let right = self.and_pred()?;
            left = binop("or", left, right);
        }
        Ok(left)
    }

    fn and_pred(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.eq_pred()?;
        while self.at(&TokKind::AmpAmp) {
            self.bump();
            let right = self.eq_pred()?;
            left = binop("and", left, right);
        }
        Ok(left)
    }

    fn eq_pred(&mut self) -> Result<Value, SurfaceError> {
        let left = self.cmp_pred()?;
        let op = match self.kind() {
            TokKind::EqEq => "eq",
            TokKind::BangEq => "neq",
            _ => return Ok(left),
        };
        self.bump();
        let right = self.cmp_pred()?;
        Ok(binop(op, left, right))
    }

    fn cmp_pred(&mut self) -> Result<Value, SurfaceError> {
        let left = self.add_pred()?;
        let op = match self.kind() {
            TokKind::Lt => "lt",
            TokKind::Le => "le",
            TokKind::Gt => "gt",
            TokKind::Ge => "ge",
            _ => return Ok(left),
        };
        self.bump();
        let right = self.add_pred()?;
        Ok(binop(op, left, right))
    }

    fn add_pred(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.mul_pred()?;
        loop {
            let op = match self.kind() {
                TokKind::Plus => "add",
                TokKind::Minus => "sub",
                _ => break,
            };
            self.bump();
            let right = self.mul_pred()?;
            left = binop(op, left, right);
        }
        Ok(left)
    }

    fn mul_pred(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.kind() {
                TokKind::Star => "mul",
                TokKind::Slash => "div",
                TokKind::Percent => "mod",
                _ => break,
            };
            self.bump();
            let right = self.unary()?;
            left = binop(op, left, right);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Bang => {
                self.bump();
                let arg = self.unary()?;
                Ok(json!({ "kind": "app", "op": "not", "args": [arg] }))
            }
            TokKind::Minus => {
                self.bump();
                let arg = self.unary()?;
                Ok(json!({ "kind": "app", "op": "neg", "args": [arg] }))
            }
            _ => self.call_pred(),
        }
    }

    fn call_pred(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            // A name (ident) or content address may be the operator of a call.
            TokKind::Ident => {
                let t = self.bump();
                match t.text.as_str() {
                    "true" => Ok(json!({ "kind": "lit", "value": true })),
                    "false" => Ok(json!({ "kind": "lit", "value": false })),
                    _ => {
                        if self.at(&TokKind::Lparen) {
                            let args = self.call_args()?;
                            Ok(json!({ "kind": "app", "op": t.text, "args": args }))
                        } else {
                            Ok(json!({ "kind": "var", "name": t.text }))
                        }
                    }
                }
            }
            TokKind::ContentAddr => {
                let t = self.bump();
                let args = if self.at(&TokKind::Lparen) {
                    self.call_args()?
                } else {
                    Vec::new()
                };
                Ok(json!({ "kind": "app", "op": t.text, "args": args }))
            }
            _ => self.atom(),
        }
    }

    fn call_args(&mut self) -> Result<Vec<Value>, SurfaceError> {
        self.expect(TokKind::Lparen, "`(` to begin argument list")?;
        let mut args: Vec<Value> = Vec::new();
        if !self.at(&TokKind::Rparen) {
            loop {
                args.push(self.parse_pred()?);
                if self.at(&TokKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(TokKind::Rparen, "`)` to close argument list")?;
        Ok(args)
    }

    fn atom(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Int => {
                let t = self.bump();
                Ok(json!({ "kind": "lit", "value": lit_int(&t.text) }))
            }
            TokKind::Float => {
                let t = self.bump();
                let f: f64 = t.text.parse().map_err(|_| {
                    SurfaceError::at(t.offset, format!("invalid float literal `{}`", t.text))
                })?;
                let n = serde_json::Number::from_f64(f)
                    .ok_or_else(|| SurfaceError::at(t.offset, "float literal is not finite"))?;
                Ok(json!({ "kind": "lit", "value": Value::Number(n) }))
            }
            TokKind::Str => {
                let t = self.bump();
                Ok(json!({ "kind": "lit", "value": t.text }))
            }
            TokKind::Lparen => {
                self.bump();
                let inner = self.parse_pred()?;
                self.expect(TokKind::Rparen, "`)` to close parenthesized predicate")?;
                Ok(inner)
            }
            _ => {
                let t = self.peek();
                Err(SurfaceError::at(
                    t.offset,
                    format!("expected a predicate, found {}", describe(t)),
                ))
            }
        }
    }
}

/// Build a JSON literal value from an integer lexeme (used inside `lit`).
fn lit_int(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        json!(i)
    } else if let Ok(u) = text.parse::<u64>() {
        json!(u)
    } else {
        // Beyond 64-bit range: fall back to float (lossy but rare for predicate
        // literals; structured big values belong in the value sub-language).
        json!(text.parse::<f64>().unwrap_or(0.0))
    }
}

// ---- pretty-printer (canonical) ----

#[derive(Clone, Copy)]
enum Fixity {
    LeftBinary,
    NonAssoc,
    Unary,
}

/// Infix/prefix rendering info for a built-in op: (symbol, precedence, fixity).
/// Higher precedence binds tighter.
fn op_info(op: &str) -> Option<(&'static str, u8, Fixity)> {
    Some(match op {
        "or" => ("||", 1, Fixity::LeftBinary),
        "and" => ("&&", 2, Fixity::LeftBinary),
        "eq" => ("==", 3, Fixity::NonAssoc),
        "neq" => ("!=", 3, Fixity::NonAssoc),
        "lt" => ("<", 4, Fixity::NonAssoc),
        "le" => ("<=", 4, Fixity::NonAssoc),
        "gt" => (">", 4, Fixity::NonAssoc),
        "ge" => (">=", 4, Fixity::NonAssoc),
        "add" => ("+", 5, Fixity::LeftBinary),
        "sub" => ("-", 5, Fixity::LeftBinary),
        "mul" => ("*", 6, Fixity::LeftBinary),
        "div" => ("/", 6, Fixity::LeftBinary),
        "mod" => ("%", 6, Fixity::LeftBinary),
        "not" => ("!", 7, Fixity::Unary),
        "neg" => ("-", 7, Fixity::Unary),
        _ => return None,
    })
}

const ATOM_PREC: u8 = 8;

fn node_kind(v: &Value) -> Option<&str> {
    v.get("kind").and_then(|k| k.as_str())
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

fn lit_to_surface(v: &Value) -> Result<String, SurfaceError> {
    match v {
        Value::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(escape_string(s)),
        Value::Null => Err(SurfaceError::msg(
            "a null literal has no v0.1 predicate surface form",
        )),
        _ => Err(SurfaceError::msg(
            "predicate `lit.value` must be a bool, number, or string on the surface",
        )),
    }
}

/// Pretty-print a predicate AST to its canonical surface string.
pub fn unparse_predicate(ast: &Value) -> Result<String, SurfaceError> {
    Ok(render(ast)?.0)
}

/// Render a node to `(text, precedence)`. The caller parenthesises via [`sub`].
fn render(node: &Value) -> Result<(String, u8), SurfaceError> {
    let kind = node_kind(node)
        .ok_or_else(|| SurfaceError::msg("predicate expression must have a `kind`"))?;
    match kind {
        "var" => {
            let name = node
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`var` missing `name`"))?;
            Ok((name.to_string(), ATOM_PREC))
        }
        "lit" => {
            let value = node
                .get("value")
                .ok_or_else(|| SurfaceError::msg("`lit` missing `value`"))?;
            Ok((lit_to_surface(value)?, ATOM_PREC))
        }
        "forall" | "exists" => {
            let vars = node
                .get("vars")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("quantifier missing `vars`"))?;
            let names: Vec<&str> = vars.iter().filter_map(|v| v.as_str()).collect();
            if names.len() != vars.len() {
                return Err(SurfaceError::msg("quantifier `vars` entries must be strings"));
            }
            let body = node
                .get("body")
                .ok_or_else(|| SurfaceError::msg("quantifier missing `body`"))?;
            // Quantifier body extends as far right as possible — printed bare.
            let body_s = render(body)?.0;
            Ok((format!("{kind} {}. {body_s}", names.join(" ")), 0))
        }
        "app" => {
            let op = node
                .get("op")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`app` missing `op`"))?;
            let args = node
                .get("args")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`app` missing `args`"))?;

            if let Some((sym, lvl, fix)) = op_info(op) {
                match fix {
                    Fixity::Unary if args.len() == 1 => {
                        let mut operand = sub(&args[0], lvl)?;
                        // Avoid emitting `--x` (which would lex as a comment).
                        if sym == "-" && operand.starts_with('-') {
                            operand = format!("({operand})");
                        }
                        return Ok((format!("{sym}{operand}"), lvl));
                    }
                    Fixity::LeftBinary if args.len() == 2 => {
                        let l = sub(&args[0], lvl)?;
                        let r = sub(&args[1], lvl + 1)?;
                        return Ok((format!("{l} {sym} {r}"), lvl));
                    }
                    Fixity::NonAssoc if args.len() == 2 => {
                        let l = sub(&args[0], lvl + 1)?;
                        let r = sub(&args[1], lvl + 1)?;
                        return Ok((format!("{l} {sym} {r}"), lvl));
                    }
                    _ => {} // arity mismatch: fall through to call syntax
                }
            }

            // Function-call form: op(arg, arg, ...). Args are full expressions.
            let parts = args
                .iter()
                .map(|a| render(a).map(|(s, _)| s))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((format!("{op}({})", parts.join(", ")), ATOM_PREC))
        }
        other => Err(SurfaceError::msg(format!(
            "unknown predicate-expression kind `{other}`"
        ))),
    }
}

/// Render `node`, wrapping in parentheses if its precedence is below `pmin`.
fn sub(node: &Value, pmin: u8) -> Result<String, SurfaceError> {
    let (s, p) = render(node)?;
    if p < pmin {
        Ok(format!("({s})"))
    } else {
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parses_to(src: &str, expected: Value) {
        assert_eq!(parse_predicate(src).unwrap(), expected, "parse of `{src}`");
    }

    #[test]
    fn parse_atoms_and_calls() {
        parses_to("x", json!({"kind": "var", "name": "x"}));
        parses_to("42", json!({"kind": "lit", "value": 42}));
        parses_to("true", json!({"kind": "lit", "value": true}));
        parses_to(
            "length(xs)",
            json!({"kind": "app", "op": "length", "args": [{"kind": "var", "name": "xs"}]}),
        );
        parses_to(
            "length(xs) == 0",
            json!({
                "kind": "app", "op": "eq",
                "args": [
                    {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "xs"}]},
                    {"kind": "lit", "value": 0}
                ]
            }),
        );
    }

    #[test]
    fn precedence_and_associativity() {
        // && binds tighter than ||
        parses_to(
            "p || q && r",
            json!({
                "kind": "app", "op": "or",
                "args": [
                    {"kind": "var", "name": "p"},
                    {"kind": "app", "op": "and", "args": [
                        {"kind": "var", "name": "q"}, {"kind": "var", "name": "r"}
                    ]}
                ]
            }),
        );
        // * binds tighter than +
        parses_to(
            "a + b * c",
            json!({
                "kind": "app", "op": "add",
                "args": [
                    {"kind": "var", "name": "a"},
                    {"kind": "app", "op": "mul", "args": [
                        {"kind": "var", "name": "b"}, {"kind": "var", "name": "c"}
                    ]}
                ]
            }),
        );
        // unary not
        parses_to(
            "!p",
            json!({"kind": "app", "op": "not", "args": [{"kind": "var", "name": "p"}]}),
        );
        // unary neg (a lone '-' before a non-digit is the neg operator)
        parses_to(
            "-x",
            json!({"kind": "app", "op": "neg", "args": [{"kind": "var", "name": "x"}]}),
        );
        // a negative number literal stays a lit, not neg
        parses_to("-7", json!({"kind": "lit", "value": -7}));
    }

    #[test]
    fn quantifiers() {
        parses_to(
            "forall xs. length(map(f, xs)) == length(xs)",
            json!({
                "kind": "forall", "vars": ["xs"],
                "body": {
                    "kind": "app", "op": "eq",
                    "args": [
                        {"kind": "app", "op": "length", "args": [
                            {"kind": "app", "op": "map", "args": [
                                {"kind": "var", "name": "f"}, {"kind": "var", "name": "xs"}
                            ]}
                        ]},
                        {"kind": "app", "op": "length", "args": [{"kind": "var", "name": "xs"}]}
                    ]
                }
            }),
        );
    }

    #[test]
    fn parse_errors() {
        assert!(parse_predicate("&& x").is_err());
        assert!(parse_predicate("f(a,").is_err());
        assert!(parse_predicate("forall . x").is_err());
        assert!(parse_predicate("(a").is_err());
    }

    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "x",
            "42",
            "-7",
            "true",
            "false",
            "\"hi\"",
            "length(xs) == 0",
            "p && q",
            "p || q && r",
            "!p && q",
            "a == b && c == d",
            "a + b * c",
            "(a + b) * c",
            "a < b",
            "x != y",
            "f(a, b)",
            "-x",
            "forall xs. length(map(f, xs)) == length(xs)",
            "exists n. n > 0",
        ] {
            let ast = parse_predicate(s).unwrap();
            let printed = unparse_predicate(&ast).unwrap();
            assert_eq!(printed, s, "unparse(parse({s:?})) should be canonical");
        }
    }

    #[test]
    fn canonical_asts_round_trip() {
        for ast in [
            json!({"kind": "var", "name": "p"}),
            json!({"kind": "lit", "value": true}),
            json!({
                "kind": "app", "op": "implies",
                "args": [{"kind": "var", "name": "p"}, {"kind": "var", "name": "q"}]
            }),
            json!({
                "kind": "app", "op": "and",
                "args": [
                    {"kind": "app", "op": "not", "args": [{"kind": "var", "name": "p"}]},
                    {"kind": "var", "name": "q"}
                ]
            }),
        ] {
            let printed = unparse_predicate(&ast).unwrap();
            let reparsed = parse_predicate(&printed).unwrap();
            assert_eq!(reparsed, ast, "parse(unparse(ast)) should equal ast");
        }
    }

    #[test]
    fn non_infix_op_uses_call_syntax() {
        // `implies` has no infix symbol, so it renders as a call.
        let ast = json!({
            "kind": "app", "op": "implies",
            "args": [{"kind": "var", "name": "p"}, {"kind": "var", "name": "q"}]
        });
        assert_eq!(unparse_predicate(&ast).unwrap(), "implies(p, q)");
    }
}
