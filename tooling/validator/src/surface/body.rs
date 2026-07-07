//! Body-expression surface syntax: parser (string -> AST) and pretty-printer
//! (AST -> canonical string).
//!
//! Grammar (from `spec/surface-syntax.md` §4, targeting
//! `body-expression.schema.json`):
//!
//! ```text
//! expr       ::= let_expr | lambda_expr | case_expr | infix_expr
//! let_expr   ::= "let" ident "=" expr "in" expr
//! lambda_expr::= "\" param* "->" expr
//! case_expr  ::= "case" expr "of" "{" arm (";" arm)* ";"? "}"
//! arm        ::= pattern "=>" expr
//! param      ::= ident | "(" ident ":" type ")"
//! infix_expr ::= <same precedence ladder as predicate §2>
//! app_expr   ::= field_expr+                      -- juxtaposition, left-assoc, curried
//! field_expr ::= atom_expr ("." ident)*
//! atom_expr  ::= ident | literal | content_addr | "(" ")" | "(" expr ")"
//! pattern    ::= "_" | ident | tag ("(" pattern ")")? | literal
//! ```
//!
//! Infix operators desugar to `app` nodes whose `fn` is a `var` naming the
//! operator (e.g. `a + b` → `app{fn: var "add", args:[a,b]}`), matching the
//! committed example bodies. Juxtaposition is curried: `f x y` →
//! `app{fn: app{fn: f, args:[x]}, args:[y]}`.
//!
//! ## Where this narrows `spec/surface-syntax.md`
//!
//! * Lambda parameter types are optional. `body-expression.schema.json` requires
//!   only `name` on each param, so both `\x -> …` (`params:[{"name":"x"}]`,
//!   inferred) and `\(x: T) -> …` (`params:[{"name":"x","type":…}]`) parse and
//!   validate. The pretty-printer preserves whichever form the AST carries, so
//!   each round-trips. (Reconciled: schema relaxed to make `type` optional.)
//! * Embedded literals in v0.1 cover scalars (`nat`/`int`/`float`/`string`/
//!   `bytes`/`bool`), `unit` (`()`), and `fn_ref` (a `fn_…` content address).
//!   Compound value literals (lists, tuples, records, variants) inside a body
//!   are deferred — the surface grammar's `value` delegation collides with `(…)`
//!   grouping, and the committed example bodies don't need them.
//! * Uppercase `Tag`s in expression position construct variants: `None` (nullary)
//!   and `Tag(expr)` with a computed payload (`Just(a / b)`). The parenthesised
//!   payload binds to the tag, so it is not read as a juxtaposition argument.

use serde_json::{json, Value};

use super::lexer::{describe, tokenize, SurfaceError, TokKind, Token};
use super::types;
use super::values;

// ---- parser ----

/// Parse a body-expression surface string into its JSON AST (conforming to
/// `spec/body-expression.schema.json`).
pub fn parse_body(src: &str) -> Result<Value, SurfaceError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, i: 0 };
    let v = p.parse_expr()?;
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

fn is_keyword(s: &str) -> bool {
    matches!(s, "let" | "in" | "of" | "case")
}

fn appfn(name: &str, args: Vec<Value>) -> Value {
    json!({ "kind": "app", "fn": { "kind": "var", "name": name }, "args": args })
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

    fn at_keyword(&self, name: &str) -> bool {
        matches!(self.kind(), TokKind::Ident) && self.peek().text == name
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

    fn expect_keyword(&mut self, name: &str) -> Result<(), SurfaceError> {
        if self.at_keyword(name) {
            self.bump();
            Ok(())
        } else {
            let t = self.peek();
            Err(SurfaceError::at(
                t.offset,
                format!("expected keyword `{name}`, found {}", describe(t)),
            ))
        }
    }

    fn parse_expr(&mut self) -> Result<Value, SurfaceError> {
        if self.at_keyword("let") {
            self.parse_let()
        } else if self.at(&TokKind::Backslash) {
            self.parse_lambda()
        } else if self.at_keyword("case") {
            self.parse_case()
        } else {
            self.or_expr()
        }
    }

    fn parse_let(&mut self) -> Result<Value, SurfaceError> {
        self.expect_keyword("let")?;
        let name = self.expect(TokKind::Ident, "a name after `let`")?;
        self.expect(TokKind::Eq, "`=` in let binding")?;
        let value = self.parse_expr()?;
        self.expect_keyword("in")?;
        let body = self.parse_expr()?;
        Ok(json!({ "kind": "let", "name": name.text, "value": value, "body": body }))
    }

    fn parse_lambda(&mut self) -> Result<Value, SurfaceError> {
        self.expect(TokKind::Backslash, "`\\` to begin a lambda")?;
        let mut params: Vec<Value> = Vec::new();
        loop {
            if self.at(&TokKind::Lparen) {
                self.bump();
                let name = self.expect(TokKind::Ident, "a parameter name")?;
                self.expect(TokKind::Colon, "`:` in typed parameter")?;
                let (ty, next) = types::parse_type_tokens(&self.toks, self.i)?;
                self.i = next;
                self.expect(TokKind::Rparen, "`)` to close typed parameter")?;
                params.push(json!({ "name": name.text, "type": ty }));
            } else if matches!(self.kind(), TokKind::Ident) && !is_keyword(&self.peek().text) {
                // Untyped parameter — the schema requires only `name`, so this
                // is schema-valid; the type is inferred. The printer renders it
                // back as a bare `name`, so it round-trips.
                let name = self.bump();
                params.push(json!({ "name": name.text }));
            } else {
                break;
            }
        }
        self.expect(TokKind::Arrow, "`->` after lambda parameters")?;
        let body = self.parse_expr()?;
        Ok(json!({ "kind": "lambda", "params": params, "body": body }))
    }

    fn parse_case(&mut self) -> Result<Value, SurfaceError> {
        self.expect_keyword("case")?;
        let scrutinee = self.parse_expr()?;
        self.expect_keyword("of")?;
        self.expect(TokKind::Lbrace, "`{` to begin case arms")?;
        let mut arms: Vec<Value> = Vec::new();
        loop {
            let pattern = self.parse_pattern()?;
            self.expect(TokKind::FatArrow, "`=>` in case arm")?;
            let body = self.parse_expr()?;
            arms.push(json!({ "pattern": pattern, "body": body }));
            if self.at(&TokKind::Semi) {
                self.bump();
                if self.at(&TokKind::Rbrace) {
                    break; // trailing semicolon
                }
            } else {
                break;
            }
        }
        self.expect(TokKind::Rbrace, "`}` to close case arms")?;
        Ok(json!({ "kind": "case", "scrutinee": scrutinee, "arms": arms }))
    }

    fn parse_pattern(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Underscore => {
                self.bump();
                Ok(json!({ "kind": "wildcard" }))
            }
            TokKind::Ident => {
                let t = self.bump();
                match t.text.as_str() {
                    "true" => {
                        Ok(json!({ "kind": "lit", "value": { "kind": "bool", "value": true } }))
                    }
                    "false" => {
                        Ok(json!({ "kind": "lit", "value": { "kind": "bool", "value": false } }))
                    }
                    // `int(N)` is the typed non-negative-`int` literal (§ expression atoms); the
                    // pretty-printer emits `int`-kind literal PATTERNS the same way, so patterns
                    // must parse it back too or the body surface doesn't round-trip.
                    "int" if self.at(&TokKind::Lparen) => {
                        self.bump(); // `(`
                        let n = self.expect(TokKind::Int, "an integer literal inside `int(...)`")?;
                        self.expect(TokKind::Rparen, "`)` to close `int(...)`")?;
                        Ok(json!({ "kind": "lit", "value": { "kind": "int", "value": values::int_from_text(&n.text) } }))
                    }
                    _ => Ok(json!({ "kind": "bind", "name": t.text })),
                }
            }
            TokKind::Tag => {
                let t = self.bump();
                let mut variant = serde_json::Map::new();
                variant.insert("kind".to_string(), Value::String("variant".to_string()));
                variant.insert("tag".to_string(), Value::String(t.text));
                if self.at(&TokKind::Lparen) {
                    self.bump();
                    let payload = self.parse_pattern()?;
                    self.expect(TokKind::Rparen, "`)` after variant pattern payload")?;
                    variant.insert("payload".to_string(), payload);
                }
                Ok(Value::Object(variant))
            }
            TokKind::Int | TokKind::Float | TokKind::Str | TokKind::Bytes => {
                let t = self.bump();
                let value =
                    values::scalar_value_from_token(&t).expect("scalar token kinds are handled")?;
                Ok(json!({ "kind": "lit", "value": value }))
            }
            _ => {
                let t = self.peek();
                Err(SurfaceError::at(
                    t.offset,
                    format!("expected a pattern, found {}", describe(t)),
                ))
            }
        }
    }

    // Infix precedence ladder (mirrors predicate §2). Each level builds
    // `app{fn: var <op>, args}` nodes.

    fn or_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.and_expr()?;
        while self.at(&TokKind::PipePipe) {
            self.bump();
            let right = self.and_expr()?;
            left = appfn("or", vec![left, right]);
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.eq_expr()?;
        while self.at(&TokKind::AmpAmp) {
            self.bump();
            let right = self.eq_expr()?;
            left = appfn("and", vec![left, right]);
        }
        Ok(left)
    }

    fn eq_expr(&mut self) -> Result<Value, SurfaceError> {
        let left = self.cmp_expr()?;
        let op = match self.kind() {
            TokKind::EqEq => "eq",
            TokKind::BangEq => "neq",
            _ => return Ok(left),
        };
        self.bump();
        let right = self.cmp_expr()?;
        Ok(appfn(op, vec![left, right]))
    }

    fn cmp_expr(&mut self) -> Result<Value, SurfaceError> {
        let left = self.add_expr()?;
        let op = match self.kind() {
            TokKind::Lt => "lt",
            TokKind::Le => "le",
            TokKind::Gt => "gt",
            TokKind::Ge => "ge",
            _ => return Ok(left),
        };
        self.bump();
        let right = self.add_expr()?;
        Ok(appfn(op, vec![left, right]))
    }

    fn add_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.mul_expr()?;
        loop {
            let op = match self.kind() {
                TokKind::Plus => "add",
                TokKind::Minus => "sub",
                _ => break,
            };
            self.bump();
            let right = self.mul_expr()?;
            left = appfn(op, vec![left, right]);
        }
        Ok(left)
    }

    fn mul_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut left = self.unary_expr()?;
        loop {
            let op = match self.kind() {
                TokKind::Star => "mul",
                TokKind::Slash => "div",
                TokKind::Percent => "mod",
                _ => break,
            };
            self.bump();
            let right = self.unary_expr()?;
            left = appfn(op, vec![left, right]);
        }
        Ok(left)
    }

    fn unary_expr(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Bang => {
                self.bump();
                let arg = self.unary_expr()?;
                Ok(appfn("not", vec![arg]))
            }
            TokKind::Minus => {
                self.bump();
                let arg = self.unary_expr()?;
                Ok(appfn("neg", vec![arg]))
            }
            _ => self.app_expr(),
        }
    }

    /// Juxtaposition application, curried and left-associative.
    fn app_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut e = self.field_expr()?;
        while self.starts_atom() {
            let arg = self.field_expr()?;
            e = json!({ "kind": "app", "fn": e, "args": [arg] });
        }
        Ok(e)
    }

    fn starts_atom(&self) -> bool {
        match self.kind() {
            TokKind::Ident => !is_keyword(&self.peek().text),
            TokKind::Int
            | TokKind::Float
            | TokKind::Str
            | TokKind::Bytes
            | TokKind::ContentAddr
            | TokKind::Lparen => true,
            _ => false,
        }
    }

    fn field_expr(&mut self) -> Result<Value, SurfaceError> {
        let mut e = self.atom_expr()?;
        while self.at(&TokKind::Dot) {
            self.bump();
            let name = self.expect(TokKind::Ident, "a field name after `.`")?;
            e = json!({ "kind": "field", "record": e, "name": name.text });
        }
        Ok(e)
    }

    fn atom_expr(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Ident => {
                let t = self.bump();
                match t.text.as_str() {
                    "true" => {
                        Ok(json!({ "kind": "lit", "value": { "kind": "bool", "value": true } }))
                    }
                    "false" => {
                        Ok(json!({ "kind": "lit", "value": { "kind": "bool", "value": false } }))
                    }
                    // `int(N)` is a typed integer literal (a non-negative `int`, distinct from the bare
                    // `nat`), mirroring the value-syntax `int(...)` form — NOT an application of a function
                    // named `int`. The pretty-printer emits non-negative `int` literals this way, so this
                    // makes the body surface round-trip.
                    "int" if self.at(&TokKind::Lparen) => {
                        self.bump(); // `(`
                        let n = self.expect(TokKind::Int, "an integer literal inside `int(...)`")?;
                        self.expect(TokKind::Rparen, "`)` to close `int(...)`")?;
                        Ok(json!({ "kind": "lit", "value": { "kind": "int", "value": values::int_from_text(&n.text) } }))
                    }
                    other if is_keyword(other) => Err(SurfaceError::at(
                        t.offset,
                        format!("unexpected keyword `{other}` in expression position"),
                    )),
                    _ => Ok(json!({ "kind": "var", "name": t.text })),
                }
            }
            TokKind::Int | TokKind::Float | TokKind::Str | TokKind::Bytes => {
                let t = self.bump();
                let value =
                    values::scalar_value_from_token(&t).expect("scalar token kinds are handled")?;
                Ok(json!({ "kind": "lit", "value": value }))
            }
            TokKind::ContentAddr => {
                let t = self.bump();
                if t.text.starts_with("fn_") {
                    Ok(json!({ "kind": "lit", "value": { "kind": "fn_ref", "target": t.text } }))
                } else {
                    Err(SurfaceError::at(
                        t.offset,
                        format!(
                            "only `fn_…` content addresses are valid in a body expression, got `{}`",
                            t.text
                        ),
                    ))
                }
            }
            TokKind::Tag => {
                // Variant construction in expression position: `None` (nullary) or `Some(expr)` (the
                // payload is an expression — e.g. `Just(a / b)`). The parenthesised payload is bound to
                // the tag here, so it is not mistaken for a juxtaposition argument.
                let t = self.bump();
                let mut variant = serde_json::Map::new();
                variant.insert("kind".to_string(), Value::String("variant".to_string()));
                variant.insert("tag".to_string(), Value::String(t.text));
                if self.at(&TokKind::Lparen) {
                    self.bump();
                    let payload = self.parse_expr()?;
                    self.expect(TokKind::Rparen, "`)` after variant payload")?;
                    variant.insert("payload".to_string(), payload);
                }
                Ok(Value::Object(variant))
            }
            TokKind::Lparen => {
                self.bump();
                if self.at(&TokKind::Rparen) {
                    self.bump();
                    return Ok(json!({ "kind": "lit", "value": { "kind": "unit" } }));
                }
                let inner = self.parse_expr()?;
                self.expect(TokKind::Rparen, "`)` to close parenthesized expression")?;
                Ok(inner)
            }
            _ => {
                let t = self.peek();
                Err(SurfaceError::at(
                    t.offset,
                    format!("expected an expression, found {}", describe(t)),
                ))
            }
        }
    }
}

// ---- pretty-printer (canonical) ----

#[derive(Clone, Copy)]
enum Fixity {
    LeftBinary,
    NonAssoc,
    Unary,
}

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

// Precedence bands used for parenthesisation.
const APP_PREC: u8 = 8;
const FIELD_PREC: u8 = 9;
const ATOM_PREC: u8 = 10;

fn node_kind(v: &Value) -> Option<&str> {
    v.get("kind").and_then(|k| k.as_str())
}

/// If `fn_node` is a `var` whose name is an infix/prefix operator, return that
/// operator name.
fn op_of_fn(fn_node: &Value) -> Option<&str> {
    if node_kind(fn_node) == Some("var") {
        let name = fn_node.get("name").and_then(|v| v.as_str())?;
        if op_info(name).is_some() {
            return Some(name);
        }
    }
    None
}

/// Pretty-print a body AST to its canonical surface string.
pub fn unparse_body(ast: &Value) -> Result<String, SurfaceError> {
    Ok(render(ast)?.0)
}

fn render(node: &Value) -> Result<(String, u8), SurfaceError> {
    let kind =
        node_kind(node).ok_or_else(|| SurfaceError::msg("body expression must have a `kind`"))?;
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
            Ok((values::unparse_value(value)?, ATOM_PREC))
        }
        "field" => {
            let record = node
                .get("record")
                .ok_or_else(|| SurfaceError::msg("`field` missing `record`"))?;
            let name = node
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`field` missing `name`"))?;
            Ok((format!("{}.{name}", sub(record, FIELD_PREC)?), FIELD_PREC))
        }
        "variant" => {
            let tag = node
                .get("tag")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`variant` missing `tag`"))?;
            match node.get("payload") {
                Some(payload) => Ok((format!("{tag}({})", render(payload)?.0), ATOM_PREC)),
                None => Ok((tag.to_string(), ATOM_PREC)),
            }
        }
        "app" => render_app(node),
        "let" => {
            let name = node
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`let` missing `name`"))?;
            let value = node
                .get("value")
                .ok_or_else(|| SurfaceError::msg("`let` missing `value`"))?;
            let body = node
                .get("body")
                .ok_or_else(|| SurfaceError::msg("`let` missing `body`"))?;
            Ok((
                format!("let {name} = {} in {}", render(value)?.0, render(body)?.0),
                0,
            ))
        }
        "lambda" => {
            let params = node
                .get("params")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`lambda` missing `params`"))?;
            let body = node
                .get("body")
                .ok_or_else(|| SurfaceError::msg("`lambda` missing `body`"))?;
            let mut rendered: Vec<String> = Vec::with_capacity(params.len());
            for p in params {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SurfaceError::msg("lambda param missing `name`"))?;
                match p.get("type") {
                    Some(ty) => rendered.push(format!("({name}: {})", types::unparse_type(ty)?)),
                    None => rendered.push(name.to_string()),
                }
            }
            Ok((
                format!("\\{} -> {}", rendered.join(" "), render(body)?.0),
                0,
            ))
        }
        "case" => {
            let scrutinee = node
                .get("scrutinee")
                .ok_or_else(|| SurfaceError::msg("`case` missing `scrutinee`"))?;
            let arms = node
                .get("arms")
                .and_then(|v| v.as_array())
                .ok_or_else(|| SurfaceError::msg("`case` missing `arms`"))?;
            let mut rendered: Vec<String> = Vec::with_capacity(arms.len());
            for arm in arms {
                let pattern = arm
                    .get("pattern")
                    .ok_or_else(|| SurfaceError::msg("case arm missing `pattern`"))?;
                let body = arm
                    .get("body")
                    .ok_or_else(|| SurfaceError::msg("case arm missing `body`"))?;
                rendered.push(format!(
                    "{} => {}",
                    render_pattern(pattern)?,
                    render(body)?.0
                ));
            }
            Ok((
                format!(
                    "case {} of {{ {} }}",
                    render(scrutinee)?.0,
                    rendered.join("; ")
                ),
                0,
            ))
        }
        other => Err(SurfaceError::msg(format!(
            "unknown body-expression kind `{other}`"
        ))),
    }
}

fn render_app(node: &Value) -> Result<(String, u8), SurfaceError> {
    let fn_node = node
        .get("fn")
        .ok_or_else(|| SurfaceError::msg("`app` missing `fn`"))?;
    let args = node
        .get("args")
        .and_then(|v| v.as_array())
        .ok_or_else(|| SurfaceError::msg("`app` missing `args`"))?;

    // Operator application renders as infix/prefix when the arity matches.
    if let Some(op) = op_of_fn(fn_node) {
        let (sym, lvl, fix) = op_info(op).expect("op_of_fn checked op_info");
        match fix {
            Fixity::Unary if args.len() == 1 => {
                let mut operand = sub(&args[0], lvl)?;
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
            _ => {} // fall through to juxtaposition
        }
    }

    // Juxtaposition: fn binds at application level, args one notch tighter.
    let mut out = sub(fn_node, APP_PREC)?;
    for a in args {
        out.push(' ');
        out.push_str(&sub(a, FIELD_PREC)?);
    }
    Ok((out, APP_PREC))
}

fn render_pattern(node: &Value) -> Result<String, SurfaceError> {
    let kind = node_kind(node).ok_or_else(|| SurfaceError::msg("pattern must have a `kind`"))?;
    match kind {
        "wildcard" => Ok("_".to_string()),
        "bind" => {
            let name = node
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`bind` pattern missing `name`"))?;
            Ok(name.to_string())
        }
        "variant" => {
            let tag = node
                .get("tag")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SurfaceError::msg("`variant` pattern missing `tag`"))?;
            match node.get("payload") {
                Some(payload) => Ok(format!("{tag}({})", render_pattern(payload)?)),
                None => Ok(tag.to_string()),
            }
        }
        "lit" => {
            let value = node
                .get("value")
                .ok_or_else(|| SurfaceError::msg("`lit` pattern missing `value`"))?;
            values::unparse_value(value)
        }
        other => Err(SurfaceError::msg(format!("unknown pattern kind `{other}`"))),
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
        assert_eq!(parse_body(src).unwrap(), expected, "parse of `{src}`");
    }

    #[test]
    fn parse_var_and_literals() {
        parses_to("x", json!({"kind": "var", "name": "x"}));
        parses_to(
            "42",
            json!({"kind": "lit", "value": {"kind": "nat", "value": 42}}),
        );
        parses_to(
            "true",
            json!({"kind": "lit", "value": {"kind": "bool", "value": true}}),
        );
        parses_to("()", json!({"kind": "lit", "value": {"kind": "unit"}}));
        // `int(N)` is a typed integer literal, not an application of a function named `int` — this is
        // what makes the body surface round-trip (the pretty-printer emits non-negative ints as `int(N)`).
        parses_to("int(1)", json!({"kind": "lit", "value": {"kind": "int", "value": 1}}));
        parses_to(
            "n + int(1)",
            json!({"kind": "app", "fn": {"kind": "var", "name": "add"}, "args": [
                {"kind": "var", "name": "n"},
                {"kind": "lit", "value": {"kind": "int", "value": 1}},
            ]}),
        );
    }

    #[test]
    fn parse_int_typed_literal_pattern() {
        // The pattern-position twin of the `int(N)` expression atom: the printer emits an
        // `int`-kind literal PATTERN as `int(N)` too, so the parser must read it back or a
        // case over int literals doesn't round-trip.
        parses_to(
            "case n of { int(2) => true; _ => false }",
            json!({"kind": "case", "scrutinee": {"kind": "var", "name": "n"}, "arms": [
                {"pattern": {"kind": "lit", "value": {"kind": "int", "value": 2}},
                 "body": {"kind": "lit", "value": {"kind": "bool", "value": true}}},
                {"pattern": {"kind": "wildcard"},
                 "body": {"kind": "lit", "value": {"kind": "bool", "value": false}}},
            ]}),
        );
        // A bare `int` (no parenthesis) in pattern position stays an ordinary binder.
        parses_to(
            "case n of { int => int; _ => n }",
            json!({"kind": "case", "scrutinee": {"kind": "var", "name": "n"}, "arms": [
                {"pattern": {"kind": "bind", "name": "int"},
                 "body": {"kind": "var", "name": "int"}},
                {"pattern": {"kind": "wildcard"},
                 "body": {"kind": "var", "name": "n"}},
            ]}),
        );
    }

    #[test]
    fn parse_infix_as_named_app() {
        parses_to(
            "n + n",
            json!({
                "kind": "app",
                "fn": {"kind": "var", "name": "add"},
                "args": [{"kind": "var", "name": "n"}, {"kind": "var", "name": "n"}]
            }),
        );
    }

    #[test]
    fn parse_juxtaposition_is_curried() {
        parses_to(
            "f x y",
            json!({
                "kind": "app",
                "fn": {
                    "kind": "app",
                    "fn": {"kind": "var", "name": "f"},
                    "args": [{"kind": "var", "name": "x"}]
                },
                "args": [{"kind": "var", "name": "y"}]
            }),
        );
    }

    #[test]
    fn parse_field_access() {
        parses_to(
            "rec.name",
            json!({"kind": "field", "record": {"kind": "var", "name": "rec"}, "name": "name"}),
        );
    }

    #[test]
    fn parse_let() {
        parses_to(
            "let y = 0 in y",
            json!({
                "kind": "let", "name": "y",
                "value": {"kind": "lit", "value": {"kind": "nat", "value": 0}},
                "body": {"kind": "var", "name": "y"}
            }),
        );
    }

    #[test]
    fn parse_typed_lambda() {
        parses_to(
            "\\(n: nat) -> n + n",
            json!({
                "kind": "lambda",
                "params": [{"name": "n", "type": {"kind": "builtin", "name": "nat"}}],
                "body": {
                    "kind": "app",
                    "fn": {"kind": "var", "name": "add"},
                    "args": [{"kind": "var", "name": "n"}, {"kind": "var", "name": "n"}]
                }
            }),
        );
    }

    #[test]
    fn parse_untyped_lambda() {
        // Untyped params carry only `name` (type inferred) and are schema-valid
        // since body-expression.schema.json requires only `name`.
        parses_to(
            "\\x -> x",
            json!({
                "kind": "lambda",
                "params": [{"name": "x"}],
                "body": {"kind": "var", "name": "x"}
            }),
        );
    }

    #[test]
    fn parse_case() {
        parses_to(
            "case n of { 0 => true; _ => false }",
            json!({
                "kind": "case",
                "scrutinee": {"kind": "var", "name": "n"},
                "arms": [
                    {
                        "pattern": {"kind": "lit", "value": {"kind": "nat", "value": 0}},
                        "body": {"kind": "lit", "value": {"kind": "bool", "value": true}}
                    },
                    {
                        "pattern": {"kind": "wildcard"},
                        "body": {"kind": "lit", "value": {"kind": "bool", "value": false}}
                    }
                ]
            }),
        );
    }

    #[test]
    fn parse_case_variant_patterns() {
        parses_to(
            "case opt of { Some(x) => x; None => 0 }",
            json!({
                "kind": "case",
                "scrutinee": {"kind": "var", "name": "opt"},
                "arms": [
                    {
                        "pattern": {"kind": "variant", "tag": "Some", "payload": {"kind": "bind", "name": "x"}},
                        "body": {"kind": "var", "name": "x"}
                    },
                    {
                        "pattern": {"kind": "variant", "tag": "None"},
                        "body": {"kind": "lit", "value": {"kind": "nat", "value": 0}}
                    }
                ]
            }),
        );
    }

    #[test]
    fn parse_variant_construction() {
        // Bare and applied tags in expression position construct variants (`None`, `Just(a / b)`).
        parses_to("None", json!({ "kind": "variant", "tag": "None" }));
        parses_to(
            "Just(a / b)",
            json!({ "kind": "variant", "tag": "Just",
                "payload": { "kind": "app", "fn": { "kind": "var", "name": "div" },
                    "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] } }),
        );
    }

    #[test]
    fn parse_errors() {
        assert!(parse_body("let x = 1").is_err()); // missing `in`
        assert!(parse_body("case n of { }").is_err()); // need >=1 arm
        assert!(parse_body("Just(").is_err()); // unterminated variant payload
        assert!(parse_body("\\(n) -> n").is_err()); // typed-param needs `: type`
    }

    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "x",
            "42",
            "true",
            "()",
            "n + n",
            "a + b * c",
            "(a + b) * c",
            "f x y",
            "f (g x)",
            "rec.name",
            "!p",
            "let y = 0 in y",
            "\\x -> x",
            "\\(n: nat) -> n + n",
            "\\(f: a -> b) (x: a) -> f x",
            "case n of { 0 => true; _ => false }",
            "case opt of { Some(x) => x; None => 0 }",
            "None",
            "Just(a / b)",
            "case b == 0 of { true => None; false => Just(a / b) }",
        ] {
            let ast = parse_body(s).unwrap();
            let printed = unparse_body(&ast).unwrap();
            assert_eq!(printed, s, "unparse(parse({s:?})) should be canonical");
        }
    }

    #[test]
    fn canonical_asts_round_trip() {
        for ast in [
            json!({"kind": "var", "name": "x"}),
            json!({"kind": "lit", "value": {"kind": "bool", "value": true}}),
            json!({
                "kind": "lambda",
                "params": [{"name": "n", "type": {"kind": "builtin", "name": "nat"}}],
                "body": {
                    "kind": "app",
                    "fn": {"kind": "var", "name": "add"},
                    "args": [{"kind": "var", "name": "n"}, {"kind": "var", "name": "n"}]
                }
            }),
            json!({
                "kind": "field",
                "record": {"kind": "var", "name": "rec"},
                "name": "age"
            }),
        ] {
            let printed = unparse_body(&ast).unwrap();
            let reparsed = parse_body(&printed).unwrap();
            assert_eq!(reparsed, ast, "parse(unparse(ast)) should equal ast");
        }
    }
}
