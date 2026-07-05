//! Type-expression surface syntax: parser (string -> AST) and pretty-printer
//! (AST -> canonical string).
//!
//! Grammar (from `spec/surface-syntax.md` §1, adapted to the committed
//! `type-expression.schema.json` AST — see the module-level note in
//! `surface/mod.rs` for where the schema overrides the spec prose):
//!
//! ```text
//! type      ::= "forall" ident+ "." type
//!             | fn_type
//! fn_type   ::= app_type ("->" app_type)*        -- arrow chain, flattened
//! app_type  ::= atom_type+                        -- juxtaposition / application
//! atom_type ::= ident                             -- var or lowercase builtin
//!             | tag                               -- builtin constructor
//!             | content_addr                      -- ref (type_… only)
//!             | "(" type ")"                      -- grouping
//!             | "(" type ("," type)+ ")"          -- tuple (2+)
//!             | "{" fields "}"                    -- record
//!             | "[" variants "]"                  -- sum
//! ```

use serde_json::{json, Value};

use super::lexer::{describe, tokenize, SurfaceError, TokKind, Token};

/// Lowercase built-in atomic types (parsed from `ident`).
fn is_atomic_builtin(s: &str) -> bool {
    matches!(
        s,
        "bool" | "int" | "nat" | "float" | "string" | "bytes" | "unit" | "never"
    )
}

/// PascalCase built-in type names (parsed from `tag`). Matches the
/// `type-expression.schema.json` enum (note: `Maybe`, not `Option`; no `IO`).
/// `Json` is nominal and nullary — a builtin atom that happens to be PascalCase.
fn is_ctor_builtin(s: &str) -> bool {
    matches!(s, "List" | "Maybe" | "Result" | "Map" | "Set" | "Json")
}

// ---- parser ----

/// Parse a type-expression surface string into its JSON AST.
///
/// The AST conforms to `spec/type-expression.schema.json`. Structural
/// well-formedness beyond the grammar (variable scoping, rank-1 polymorphism,
/// field/tag uniqueness) is **not** checked here — run
/// [`crate::check_type_well_formed`] on the result for that.
pub fn parse_type(src: &str) -> Result<Value, SurfaceError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks: &toks, i: 0 };
    let v = p.parse_type()?;
    if !p.at(&TokKind::Eof) {
        let t = p.peek();
        return Err(SurfaceError::at(
            t.offset,
            format!("unexpected trailing input: {}", describe(t)),
        ));
    }
    Ok(v)
}

/// Parse one type expression from an existing token slice, starting at index
/// `start`. Returns the AST and the index of the next unconsumed token. Used by
/// the body sub-language to parse lambda-parameter type annotations from a
/// shared token stream. Does not require reaching EOF.
pub(crate) fn parse_type_tokens(
    toks: &[Token],
    start: usize,
) -> Result<(Value, usize), SurfaceError> {
    let mut p = Parser { toks, i: start };
    let v = p.parse_type()?;
    Ok((v, p.i))
}

struct Parser<'a> {
    toks: &'a [Token],
    i: usize,
}

impl Parser<'_> {
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

    fn parse_type(&mut self) -> Result<Value, SurfaceError> {
        if self.at_keyword("forall") {
            self.bump();
            let mut vars: Vec<String> = Vec::new();
            while matches!(self.kind(), TokKind::Ident) {
                vars.push(self.bump().text);
            }
            if vars.is_empty() {
                let t = self.peek();
                return Err(SurfaceError::at(
                    t.offset,
                    format!(
                        "expected at least one type variable after `forall`, found {}",
                        describe(t)
                    ),
                ));
            }
            self.expect(TokKind::Dot, "`.` after forall binders")?;
            let body = self.parse_type()?;
            return Ok(json!({ "kind": "forall", "vars": vars, "body": body }));
        }
        self.parse_fn()
    }

    /// Arrow chain, flattened into the schema's multi-arg `fn`. `a -> b -> c`
    /// becomes `{params:[a,b], result:c}` (the canonical, result-not-a-fn form).
    fn parse_fn(&mut self) -> Result<Value, SurfaceError> {
        let first = self.parse_app()?;
        if !self.at(&TokKind::Arrow) {
            return Ok(first);
        }
        let mut parts = vec![first];
        while self.at(&TokKind::Arrow) {
            self.bump();
            parts.push(self.parse_app()?);
        }
        let result = parts.pop().expect("arrow chain has at least two parts");
        Ok(json!({ "kind": "fn", "params": parts, "result": result }))
    }

    /// Juxtaposition: `f a b` -> `apply{ctor:f, args:[a,b]}`. A lone atom passes
    /// through unwrapped.
    fn parse_app(&mut self) -> Result<Value, SurfaceError> {
        let head = self.parse_atom()?;
        let mut args: Vec<Value> = Vec::new();
        while self.starts_atom() {
            args.push(self.parse_atom()?);
        }
        if args.is_empty() {
            Ok(head)
        } else {
            Ok(json!({ "kind": "apply", "ctor": head, "args": args }))
        }
    }

    fn starts_atom(&self) -> bool {
        matches!(
            self.kind(),
            TokKind::Ident
                | TokKind::Tag
                | TokKind::ContentAddr
                | TokKind::Lparen
                | TokKind::Lbrace
                | TokKind::Lbracket
        )
    }

    fn parse_atom(&mut self) -> Result<Value, SurfaceError> {
        match self.kind() {
            TokKind::Ident => {
                let t = self.bump();
                if is_atomic_builtin(&t.text) {
                    Ok(json!({ "kind": "builtin", "name": t.text }))
                } else {
                    Ok(json!({ "kind": "var", "name": t.text }))
                }
            }
            TokKind::Tag => {
                let t = self.bump();
                if is_ctor_builtin(&t.text) {
                    Ok(json!({ "kind": "builtin", "name": t.text }))
                } else {
                    Err(SurfaceError::at(
                        t.offset,
                        format!(
                            "`{}` is not a known type constructor; uppercase type variables are reserved for future higher-kinded types (v0.1 type variables are lowercase)",
                            t.text
                        ),
                    ))
                }
            }
            TokKind::ContentAddr => {
                let t = self.bump();
                if t.text.starts_with("type_") {
                    Ok(json!({ "kind": "ref", "target": t.text }))
                } else {
                    Err(SurfaceError::at(
                        t.offset,
                        format!(
                            "only `type_…` content addresses are valid in a type position, got `{}`",
                            t.text
                        ),
                    ))
                }
            }
            TokKind::Lparen => {
                self.bump();
                let first = self.parse_type()?;
                if self.at(&TokKind::Comma) {
                    let mut elems = vec![first];
                    while self.at(&TokKind::Comma) {
                        self.bump();
                        elems.push(self.parse_type()?);
                    }
                    self.expect(TokKind::Rparen, "`)` to close tuple")?;
                    Ok(json!({ "kind": "tuple", "elems": elems }))
                } else {
                    self.expect(TokKind::Rparen, "`)` to close parenthesized type")?;
                    Ok(first)
                }
            }
            TokKind::Lbrace => self.parse_record(),
            TokKind::Lbracket => self.parse_sum(),
            _ => {
                let t = self.peek();
                Err(SurfaceError::at(
                    t.offset,
                    format!("expected a type, found {}", describe(t)),
                ))
            }
        }
    }

    fn parse_record(&mut self) -> Result<Value, SurfaceError> {
        self.expect(TokKind::Lbrace, "`{`")?;
        let mut fields: Vec<Value> = Vec::new();
        if !self.at(&TokKind::Rbrace) {
            loop {
                let name = self.expect(TokKind::Ident, "a field name")?;
                self.expect(TokKind::Colon, "`:` after field name")?;
                let ty = self.parse_type()?;
                fields.push(json!({ "name": name.text, "type": ty }));
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

    fn parse_sum(&mut self) -> Result<Value, SurfaceError> {
        self.expect(TokKind::Lbracket, "`[`")?;
        let mut variants: Vec<Value> = Vec::new();
        if !self.at(&TokKind::Rbracket) {
            loop {
                let tag = self.expect(TokKind::Tag, "a variant tag")?;
                let mut variant = serde_json::Map::new();
                variant.insert("tag".to_string(), Value::String(tag.text));
                if self.at(&TokKind::Lparen) {
                    self.bump();
                    let ty = self.parse_type()?;
                    self.expect(TokKind::Rparen, "`)` after variant payload")?;
                    variant.insert("type".to_string(), ty);
                }
                variants.push(Value::Object(variant));
                if self.at(&TokKind::Pipe) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(TokKind::Rbracket, "`]` to close sum")?;
        Ok(json!({ "kind": "sum", "variants": variants }))
    }
}

// ---- pretty-printer (canonical) ----

fn node_kind(v: &Value) -> Option<&str> {
    v.get("kind").and_then(|k| k.as_str())
}

fn get_str<'a>(v: &'a Value, key: &str) -> Result<&'a str, SurfaceError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| SurfaceError::msg(format!("type node missing string field `{key}`")))
}

fn get_arr<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, SurfaceError> {
    v.get(key)
        .and_then(|x| x.as_array())
        .ok_or_else(|| SurfaceError::msg(format!("type node missing array field `{key}`")))
}

/// Pretty-print a type-expression AST to its canonical surface string.
///
/// Canonical form per `spec/surface-syntax.md` §1: function params that are
/// themselves functions are parenthesised; `apply` args that are functions or
/// applications are parenthesised; record fields are sorted by name; sum
/// variants and tuple elements keep declaration order.
pub fn unparse_type(ast: &Value) -> Result<String, SurfaceError> {
    let kind = node_kind(ast)
        .ok_or_else(|| SurfaceError::msg("type expression must be a JSON object with a `kind`"))?;

    match kind {
        "builtin" | "var" => Ok(get_str(ast, "name")?.to_string()),
        "ref" => Ok(get_str(ast, "target")?.to_string()),
        "forall" => {
            let vars = get_arr(ast, "vars")?;
            let names: Vec<&str> = vars.iter().filter_map(|v| v.as_str()).collect();
            if names.len() != vars.len() {
                return Err(SurfaceError::msg("`forall.vars` entries must be strings"));
            }
            let body = ast
                .get("body")
                .ok_or_else(|| SurfaceError::msg("`forall` missing `body`"))?;
            Ok(format!(
                "forall {}. {}",
                names.join(" "),
                unparse_type(body)?
            ))
        }
        "fn" => {
            let params = get_arr(ast, "params")?;
            let result = ast
                .get("result")
                .ok_or_else(|| SurfaceError::msg("`fn` missing `result`"))?;
            let mut parts: Vec<String> = Vec::with_capacity(params.len() + 1);
            for p in params {
                let s = unparse_type(p)?;
                if node_kind(p) == Some("fn") {
                    parts.push(format!("({s})"));
                } else {
                    parts.push(s);
                }
            }
            parts.push(unparse_type(result)?);
            Ok(parts.join(" -> "))
        }
        "apply" => {
            let ctor = ast
                .get("ctor")
                .ok_or_else(|| SurfaceError::msg("`apply` missing `ctor`"))?;
            let args = get_arr(ast, "args")?;
            let mut out = String::new();
            let cs = unparse_type(ctor)?;
            // A constructor is normally a builtin/var/ref; defensively wrap an
            // (ill-formed) fn ctor in parens.
            if node_kind(ctor) == Some("fn") {
                out.push_str(&format!("({cs})"));
            } else {
                out.push_str(&cs);
            }
            for a in args {
                out.push(' ');
                let as_ = unparse_type(a)?;
                if matches!(node_kind(a), Some("fn") | Some("apply")) {
                    out.push_str(&format!("({as_})"));
                } else {
                    out.push_str(&as_);
                }
            }
            Ok(out)
        }
        "tuple" => {
            let elems = get_arr(ast, "elems")?;
            let parts = elems
                .iter()
                .map(unparse_type)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(", ")))
        }
        "record" => {
            let fields = get_arr(ast, "fields")?;
            let mut pairs: Vec<(String, String)> = Vec::with_capacity(fields.len());
            for f in fields {
                let name = get_str(f, "name")?.to_string();
                let ty = f
                    .get("type")
                    .ok_or_else(|| SurfaceError::msg("record field missing `type`"))?;
                pairs.push((name, unparse_type(ty)?));
            }
            // Canonical form sorts record fields by name.
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let body = pairs
                .into_iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(format!("{{{body}}}"))
        }
        "sum" => {
            let variants = get_arr(ast, "variants")?;
            let mut parts: Vec<String> = Vec::with_capacity(variants.len());
            for v in variants {
                let tag = get_str(v, "tag")?;
                if let Some(ty) = v.get("type") {
                    parts.push(format!("{tag}({})", unparse_type(ty)?));
                } else {
                    parts.push(tag.to_string());
                }
            }
            Ok(format!("[{}]", parts.join(" | ")))
        }
        other => Err(SurfaceError::msg(format!(
            "unknown type-expression kind `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parses_to(src: &str, expected: Value) {
        assert_eq!(parse_type(src).unwrap(), expected, "parse of `{src}`");
    }

    #[test]
    fn parse_atoms() {
        parses_to("int", json!({"kind": "builtin", "name": "int"}));
        parses_to("a", json!({"kind": "var", "name": "a"}));
        let hex = "0".repeat(64);
        parses_to(
            &format!("type_{hex}"),
            json!({"kind": "ref", "target": format!("type_{hex}")}),
        );
    }

    #[test]
    fn parse_apply_fn_and_quantifier() {
        parses_to(
            "List a",
            json!({
                "kind": "apply",
                "ctor": {"kind": "builtin", "name": "List"},
                "args": [{"kind": "var", "name": "a"}]
            }),
        );
        parses_to(
            "a -> b",
            json!({
                "kind": "fn",
                "params": [{"kind": "var", "name": "a"}],
                "result": {"kind": "var", "name": "b"}
            }),
        );
        // Arrow chains flatten; a parenthesised arrow stays a nested fn param.
        parses_to(
            "(a -> b) -> c",
            json!({
                "kind": "fn",
                "params": [{
                    "kind": "fn",
                    "params": [{"kind": "var", "name": "a"}],
                    "result": {"kind": "var", "name": "b"}
                }],
                "result": {"kind": "var", "name": "c"}
            }),
        );
        parses_to(
            "forall a. a",
            json!({"kind": "forall", "vars": ["a"], "body": {"kind": "var", "name": "a"}}),
        );
    }

    #[test]
    fn parse_compound_atoms() {
        parses_to(
            "(int, bool)",
            json!({
                "kind": "tuple",
                "elems": [
                    {"kind": "builtin", "name": "int"},
                    {"kind": "builtin", "name": "bool"}
                ]
            }),
        );
        parses_to(
            "{name: string, age: nat}",
            json!({
                "kind": "record",
                "fields": [
                    {"name": "name", "type": {"kind": "builtin", "name": "string"}},
                    {"name": "age", "type": {"kind": "builtin", "name": "nat"}}
                ]
            }),
        );
        parses_to(
            "[None | Some(a)]",
            json!({
                "kind": "sum",
                "variants": [
                    {"tag": "None"},
                    {"tag": "Some", "type": {"kind": "var", "name": "a"}}
                ]
            }),
        );
    }

    #[test]
    fn parse_errors_carry_offsets() {
        // Unknown constructor / uppercase type variable.
        assert!(parse_type("Foo").is_err());
        // A bare integer is not a type.
        assert!(parse_type("123").is_err());
        // Dangling arrow.
        assert!(parse_type("a ->").is_err());
        // Unbalanced paren.
        let e = parse_type("(a").unwrap_err();
        assert!(e.offset.is_some());
        // Non-type content address in a type position.
        let hex = "0".repeat(64);
        assert!(parse_type(&format!("fn_{hex}")).is_err());
    }

    /// Strings already in canonical form must round-trip unchanged.
    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "int",
            "a",
            "List a",
            "Map k v",
            "a -> b",
            "(a -> b) -> c",
            "List a -> List b",
            "forall a b. (a -> b) -> List a -> List b",
            "(int, bool)",
            "{age: nat, name: string}",
            "[None | Some(a)]",
            "List (List a)",
            "Json",
            "List string -> Json -> [Just(Json) | None]",
        ] {
            let ast = parse_type(s).unwrap();
            let printed = unparse_type(&ast).unwrap();
            assert_eq!(printed, s, "unparse(parse({s:?})) should be canonical");
        }
    }

    /// Canonical ASTs must round-trip through unparse and back.
    #[test]
    fn canonical_asts_round_trip() {
        for ast in [
            json!({"kind": "builtin", "name": "nat"}),
            json!({
                "kind": "fn",
                "params": [{"kind": "var", "name": "a"}, {"kind": "var", "name": "b"}],
                "result": {"kind": "var", "name": "c"}
            }),
            json!({
                "kind": "forall",
                "vars": ["a"],
                "body": {
                    "kind": "apply",
                    "ctor": {"kind": "builtin", "name": "List"},
                    "args": [{"kind": "var", "name": "a"}]
                }
            }),
            json!({
                "kind": "record",
                "fields": [
                    {"name": "age", "type": {"kind": "builtin", "name": "nat"}},
                    {"name": "name", "type": {"kind": "builtin", "name": "string"}}
                ]
            }),
        ] {
            let printed = unparse_type(&ast).unwrap();
            let reparsed = parse_type(&printed).unwrap();
            assert_eq!(reparsed, ast, "parse(unparse(ast)) should equal ast");
        }
    }

    /// Unparse normalises non-canonical record field order to sorted.
    #[test]
    fn unparse_sorts_record_fields() {
        let ast = parse_type("{name: string, age: nat}").unwrap();
        assert_eq!(unparse_type(&ast).unwrap(), "{age: nat, name: string}");
    }
}
