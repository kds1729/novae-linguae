//! nl-ingest: Parse public Rust functions and emit Nova Lingua v0.1 function records.
//!
//! Each public top-level `pub fn` in the given source files becomes one JSON record on
//! stdout (JSONL by default, `--pretty` for readable). The record satisfies the
//! function-record.schema.json structural requirements; hash and body_hash are real
//! BLAKE3 digests, though body_hash is the hash of the function's token stream rather
//! than a proper Nova Lingua body-expression AST — that translation is future work.
//!
//! With `--v2`, a function that has usable `///` doc-tests is emitted as a higher-fidelity v0.2
//! record instead: `signature.type` is a structured type-expression AST built from the syn types
//! (unknown/`impl Trait`/user types become fresh forall-bound type variables — there is no "unknown"
//! builtin), and `examples` are REAL value-expression ASTs extracted from `assert_eq!(...)` lines in
//! the doc-tests (nothing is executed). Functions without usable doc-tests fall back to a v0.1 record.
//!
//! CAVEATS (all addressable in future iterations):
//!   - Top-level `pub fn` items AND public methods of inherent `impl` blocks are ingested; for a
//!     method the receiver (`self`) is treated as the first parameter (`arg0`, UFCS convention), so
//!     `xs.reverse()` records like `reverse(xs)`. Trait-impl methods (Clone::clone, Iterator::next,
//!     ...) are skipped — they are the trait's API surface, not the type's own.
//!   - v0.1 mode: `examples.args` is one null per parameter and `result` is null (fill in later);
//!     types render as a flavored string. `--v2` fixes both for doc-tested functions.
//!   - `signature.terminates` is always "unknown". Static analysis is future work.
//!   - `effects`, `properties`, `intent_tags` are empty; add them after ingestion.
//!   - Generic lifetime params are stripped from the type.

use anyhow::{Context, Result};
use clap::Parser;
use nl_validator::{blake3_hash, canonicalize, format_hash};
use quote::ToTokens;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::{fs, path::PathBuf};
use syn::punctuated::Punctuated;
use syn::{FnArg, Item, ReturnType};

#[derive(Parser)]
#[command(
    name = "nl-ingest",
    about = "Parse public Rust functions and emit Nova Lingua v0.1 function records (JSONL)"
)]
struct Cli {
    /// One or more .rs source files to ingest
    files: Vec<PathBuf>,

    /// Crate name added to name_hints as '<crate>::<fn>' alongside the bare fn name
    #[arg(long)]
    crate_name: Option<String>,

    /// Pretty-print each record (default: compact JSONL, one record per line)
    #[arg(long)]
    pretty: bool,

    /// Higher fidelity: emit v0.2 records (structured type AST + real examples from `///` doc-tests)
    /// for functions with usable doc-tests; v0.1 otherwise.
    #[arg(long)]
    v2: bool,

    /// Attach curated algebraic laws (property_catalog.json) to recognised functions
    /// (map/filter/sort/reverse/id, ...); implies --v2. Verify with `nl-validator check-properties`.
    #[arg(long)]
    properties: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.files.is_empty() {
        eprintln!("nl-ingest: no files given — pass one or more .rs paths");
        std::process::exit(1);
    }
    for path in &cli.files {
        let source =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let ast = syn::parse_file(&source)
            .with_context(|| format!("parsing {}", path.display()))?;
        let v2 = cli.v2 || cli.properties;
        let crate_name = cli.crate_name.as_deref();
        for item in ast.items {
            match item {
                // Top-level public functions.
                Item::Fn(func) if matches!(func.vis, syn::Visibility::Public(_)) => {
                    let record = build_one(&func, crate_name, v2, cli.properties, false)
                        .with_context(|| format!("building record for `{}` in {}", func.sig.ident, path.display()))?;
                    print_record(&record, cli.pretty)?;
                }
                // Public methods of inherent `impl` blocks (receiver -> arg0). Trait *impls* are
                // skipped: their methods (Clone::clone, Iterator::next, ...) are the trait's API.
                Item::Impl(imp) if imp.trait_.is_none() => {
                    for ii in &imp.items {
                        let syn::ImplItem::Fn(m) = ii else { continue };
                        if !matches!(m.vis, syn::Visibility::Public(_)) {
                            continue;
                        }
                        let Some(func) = lift_method(&m.attrs, &m.vis, &m.sig, Some(&m.block), &imp.self_ty, &imp.generics)
                        else { continue };
                        let record = build_one(&func, crate_name, v2, cli.properties, true)
                            .with_context(|| format!("building record for method `{}` in {}", m.sig.ident, path.display()))?;
                        print_record(&record, cli.pretty)?;
                    }
                }
                // Methods *declared* on a public trait (the canonical iterator-method home:
                // `trait Iterator { fn map(self, f) -> ...; }`). Receiver -> arg0; Self is the type var.
                Item::Trait(tr) if matches!(tr.vis, syn::Visibility::Public(_)) => {
                    let self_ty: syn::Type = syn::parse_quote!(Self);
                    let pub_vis: syn::Visibility = syn::parse_quote!(pub);
                    for ti in &tr.items {
                        let syn::TraitItem::Fn(m) = ti else { continue };
                        let Some(func) = lift_method(&m.attrs, &pub_vis, &m.sig, m.default.as_ref(), &self_ty, &tr.generics)
                        else { continue };
                        let record = build_one(&func, crate_name, v2, cli.properties, true)
                            .with_context(|| format!("building record for trait method `{}` in {}", m.sig.ident, path.display()))?;
                        print_record(&record, cli.pretty)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn print_record(record: &Value, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(record)?);
    } else {
        println!("{}", serde_json::to_string(record)?);
    }
    Ok(())
}

fn build_record(func: &syn::ItemFn, crate_name: Option<&str>) -> Result<Value> {
    let fn_name = func.sig.ident.to_string();

    // name_hints: bare snake_case name, optionally also "crate::name"
    let mut name_hints: Vec<Value> = vec![json!(fn_name)];
    if let Some(krate) = crate_name {
        // name_hint pattern is ^[a-z][a-z0-9_]*$ — use underscore form, not ::
        name_hints.push(json!(format!("{}_{}", krate, fn_name)));
    }

    // body_hash: BLAKE3 of the function body's normalised token stream.
    // This is a synthetic expr_ address — the body is not a Nova Lingua AST yet.
    let body_tokens = func.block.to_token_stream().to_string();
    let body_hash = format_hash("expr", &blake3_hash(body_tokens.as_bytes()));

    // One placeholder example per function: args = [null, null, ...], result = null.
    // The arity is correct; the values are placeholders to be filled in after ingestion.
    let arity = func
        .sig
        .inputs
        .iter()
        .filter(|a| matches!(a, FnArg::Typed(_)))
        .count();
    let example = json!({
        "args": vec![Value::Null; arity],
        "result": null
    });

    // Build the record with a placeholder hash — real hash computed below.
    let mut record = json!({
        "schema_version": "0.1.0",
        "hash": "fn_0000000000000000000000000000000000000000000000000000000000000000",
        "name_hints": name_hints,
        "signature": {
            "type": format_sig(&func.sig),
            "refinements": [],
            "effects":      [],
            "capabilities": [],
            "terminates":   "unknown"
        },
        "examples":    [example],
        "properties":  [],
        "intent_tags": [],
        "derived_from": null,
        "supersedes":   null,
        "body_hash": body_hash
    });

    // Compute and insert the real hash (strip hash field → JCS → BLAKE3).
    record["hash"] = json!(fn_hash(&record)?);
    Ok(record)
}

/// Dispatch: a v0.2 record when `--v2` and the function has usable doc-test examples, else v0.1.
/// `is_method` marks records lifted from `impl` methods (receiver = arg0) for the property catalog.
fn build_one(func: &syn::ItemFn, crate_name: Option<&str>, v2: bool, properties: bool, is_method: bool) -> Result<Value> {
    if v2 {
        if let Some(rec) = build_v2_record(func, crate_name, properties, is_method)? {
            return Ok(rec);
        }
    }
    build_record(func, crate_name)
}

/// Lift a method into a synthetic free function so the whole record pipeline can reuse it: the
/// receiver (`self` / `&self` / `&mut self`) becomes a typed first parameter `__self` of `self_ty`
/// (the impl's `Self` for inherent methods, or `Self` for trait methods), and the enclosing generics
/// are merged in. `block` is the method body, or `None` for a body-less trait method (a synthetic
/// `unimplemented!()` body stands in — only the signature/doc/examples are used). Returns None for an
/// associated function with no receiver (a constructor etc.) — that isn't a method.
fn lift_method(
    attrs: &[syn::Attribute],
    vis: &syn::Visibility,
    sig: &syn::Signature,
    block: Option<&syn::Block>,
    self_ty: &syn::Type,
    outer_generics: &syn::Generics,
) -> Option<syn::ItemFn> {
    if !matches!(sig.inputs.first(), Some(syn::FnArg::Receiver(_))) {
        return None;
    }
    let mut sig = sig.clone();
    let self_param: syn::FnArg = syn::parse_quote!(__self: #self_ty);
    if let Some(first) = sig.inputs.first_mut() {
        *first = self_param;
    }
    let mut params = outer_generics.params.clone();
    params.extend(sig.generics.params.clone());
    sig.generics.params = params;
    let block = block.cloned().unwrap_or_else(|| syn::parse_quote!({ unimplemented!() }));
    Some(syn::ItemFn { attrs: attrs.to_vec(), vis: vis.clone(), sig, block: Box::new(block) })
}

// --- v0.2: structured type AST (from syn) + real examples from `///` doc-tests ----------------

fn t_var(name: &str) -> Value {
    json!({ "kind": "var", "name": name })
}
fn t_builtin(name: &str) -> Value {
    json!({ "kind": "builtin", "name": name })
}
fn t_apply(ctor: &str, args: Vec<Value>) -> Value {
    json!({ "kind": "apply", "ctor": { "kind": "builtin", "name": ctor }, "args": args })
}

/// Allocates type-variable names: stable per source name (a generic param used twice -> one var),
/// fresh per genuinely-unknown position. `used` collects them for the enclosing forall.
struct VarCtx {
    named: HashMap<String, String>,
    used: Vec<String>,
    n: usize,
}
impl VarCtx {
    fn new() -> Self {
        Self { named: HashMap::new(), used: Vec::new(), n: 0 }
    }
    fn alloc(&mut self) -> String {
        let i = self.n;
        self.n += 1;
        let letter = (b'a' + (i % 26) as u8) as char;
        if i < 26 { letter.to_string() } else { format!("{}{}", letter, i / 26) }
    }
    fn note(&mut self, v: String) -> String {
        if !self.used.contains(&v) {
            self.used.push(v.clone());
        }
        v
    }
    fn fresh(&mut self) -> String {
        let v = self.alloc();
        self.note(v)
    }
    fn named(&mut self, name: &str) -> String {
        if !self.named.contains_key(name) {
            let v = self.alloc();
            self.named.insert(name.to_string(), v);
        }
        let v = self.named[name].clone();
        self.note(v)
    }
}

fn atomic_builtin(ident: &str) -> Option<&'static str> {
    match ident {
        "bool" => Some("bool"),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => Some("int"),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => Some("nat"),
        "f32" | "f64" => Some("float"),
        "str" | "String" | "char" => Some("string"),
        _ => None,
    }
}

fn ty_to_ast(ty: &syn::Type, generics: &HashSet<String>, ctx: &mut VarCtx) -> Value {
    match ty {
        syn::Type::Reference(r) => ty_to_ast(&r.elem, generics, ctx),
        syn::Type::Paren(p) => ty_to_ast(&p.elem, generics, ctx),
        syn::Type::Group(g) => ty_to_ast(&g.elem, generics, ctx),
        syn::Type::Slice(s) => t_apply("List", vec![ty_to_ast(&s.elem, generics, ctx)]),
        syn::Type::Array(a) => t_apply("List", vec![ty_to_ast(&a.elem, generics, ctx)]),
        syn::Type::Tuple(t) => {
            if t.elems.is_empty() {
                t_builtin("unit")
            } else if t.elems.len() == 1 {
                ty_to_ast(&t.elems[0], generics, ctx)
            } else {
                let mut elems = Vec::new();
                for e in &t.elems {
                    elems.push(ty_to_ast(e, generics, ctx));
                }
                json!({ "kind": "tuple", "elems": elems })
            }
        }
        syn::Type::BareFn(bf) => {
            let mut params = Vec::new();
            for a in &bf.inputs {
                params.push(ty_to_ast(&a.ty, generics, ctx));
            }
            let result = match &bf.output {
                ReturnType::Default => t_builtin("unit"),
                ReturnType::Type(_, t) => ty_to_ast(t, generics, ctx),
            };
            json!({ "kind": "fn", "params": params, "result": result })
        }
        syn::Type::Path(tp) => path_to_ast(tp, generics, ctx),
        _ => t_var(&ctx.fresh()),
    }
}

fn path_to_ast(tp: &syn::TypePath, generics: &HashSet<String>, ctx: &mut VarCtx) -> Value {
    let seg = match tp.path.segments.last() {
        Some(s) => s,
        None => return t_var(&ctx.fresh()),
    };
    let ident = seg.ident.to_string();
    let mut argtys: Vec<&syn::Type> = Vec::new();
    if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
        for a in &ab.args {
            if let syn::GenericArgument::Type(t) = a {
                argtys.push(t);
            }
        }
    }
    let mut args: Vec<Value> = Vec::new();
    for t in &argtys {
        args.push(ty_to_ast(t, generics, ctx));
    }

    if args.is_empty() {
        if generics.contains(&ident) {
            return t_var(&ctx.named(&ident));
        }
        if let Some(b) = atomic_builtin(&ident) {
            return t_builtin(b);
        }
        return t_var(&ctx.fresh()); // unknown concrete (user) type
    }
    match ident.as_str() {
        "Box" | "Rc" | "Arc" | "Cell" | "RefCell" => args.into_iter().next().unwrap(),
        "Vec" | "VecDeque" | "BinaryHeap" | "LinkedList" => {
            t_apply("List", vec![args.into_iter().next().unwrap()])
        }
        "Option" => t_apply("Maybe", vec![args.into_iter().next().unwrap()]),
        "HashSet" | "BTreeSet" => t_apply("Set", vec![args.into_iter().next().unwrap()]),
        "HashMap" | "BTreeMap" if args.len() >= 2 => {
            let mut it = args.into_iter();
            t_apply("Map", vec![it.next().unwrap(), it.next().unwrap()])
        }
        "Result" if args.len() >= 2 => {
            let mut it = args.into_iter();
            t_apply("Result", vec![it.next().unwrap(), it.next().unwrap()])
        }
        _ => t_var(&ctx.fresh()), // unknown generic constructor
    }
}

fn type_ast_from_sig(sig: &syn::Signature) -> Value {
    let generics: HashSet<String> = sig
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            syn::GenericParam::Type(tp) => Some(tp.ident.to_string()),
            _ => None,
        })
        .collect();
    let mut ctx = VarCtx::new();
    let mut params = Vec::new();
    for arg in &sig.inputs {
        if let FnArg::Typed(pt) = arg {
            params.push(ty_to_ast(&pt.ty, &generics, &mut ctx));
        }
    }
    let result = match &sig.output {
        ReturnType::Default => t_builtin("unit"),
        ReturnType::Type(_, ty) => ty_to_ast(ty, &generics, &mut ctx),
    };
    let fnt = json!({ "kind": "fn", "params": params, "result": result });
    let mut vars = ctx.used.clone();
    vars.sort();
    vars.dedup();
    if vars.is_empty() {
        fnt
    } else {
        json!({ "kind": "forall", "vars": vars, "body": fnt })
    }
}

/// (param types, result type) from a (possibly forall-wrapped) fn type AST — value-encoding hints.
fn split_fn_type(t: &Value) -> (Vec<Value>, Option<Value>) {
    let body = if t["kind"] == "forall" { &t["body"] } else { t };
    if body["kind"] == "fn" {
        (
            body["params"].as_array().cloned().unwrap_or_default(),
            Some(body["result"].clone()),
        )
    } else {
        (Vec::new(), None)
    }
}

fn is_nat_hint(h: Option<&Value>) -> bool {
    h.map(|v| v["kind"] == "builtin" && v["name"] == "nat").unwrap_or(false)
}

fn int_value(n: i64) -> Value {
    if n.unsigned_abs() < (1u64 << 53) { json!(n) } else { json!(n.to_string()) }
}

fn lit_value(lit: &syn::Lit, hint: Option<&Value>, neg: bool) -> Option<Value> {
    match lit {
        syn::Lit::Bool(b) => Some(json!({ "kind": "bool", "value": b.value })),
        syn::Lit::Str(s) => Some(json!({ "kind": "string", "value": s.value() })),
        syn::Lit::Char(c) => Some(json!({ "kind": "string", "value": c.value().to_string() })),
        syn::Lit::Int(i) => {
            let mut n: i64 = i.base10_parse().ok()?;
            if neg {
                n = -n;
            }
            if !neg && is_nat_hint(hint) && n >= 0 {
                Some(json!({ "kind": "nat", "value": int_value(n) }))
            } else {
                Some(json!({ "kind": "int", "value": int_value(n) }))
            }
        }
        syn::Lit::Float(f) => {
            let mut v: f64 = f.base10_parse().ok()?;
            if neg {
                v = -v;
            }
            Some(json!({ "kind": "float", "value": v }))
        }
        _ => None,
    }
}

fn list_elem_hint(h: Option<&Value>) -> Option<&Value> {
    h.and_then(|v| {
        if v["kind"] == "apply" && v["ctor"]["name"] == "List" {
            v["args"].get(0)
        } else {
            None
        }
    })
}

/// Encode a Rust literal expression as a value-expression AST. Returns None for anything not
/// faithfully representable (so the example is skipped — never fabricated).
fn value_ast(expr: &syn::Expr, hint: Option<&Value>) -> Option<Value> {
    match expr {
        syn::Expr::Lit(el) => lit_value(&el.lit, hint, false),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => match &*u.expr {
            syn::Expr::Lit(el) => lit_value(&el.lit, hint, true),
            _ => None,
        },
        syn::Expr::Group(g) => value_ast(&g.expr, hint),
        syn::Expr::Paren(p) => value_ast(&p.expr, hint),
        syn::Expr::Array(a) => {
            let eh = list_elem_hint(hint);
            let mut elems = Vec::new();
            for e in &a.elems {
                elems.push(value_ast(e, eh)?);
            }
            Some(json!({ "kind": "list", "elems": elems }))
        }
        syn::Expr::Tuple(t) => {
            if t.elems.is_empty() {
                Some(json!({ "kind": "unit" }))
            } else if t.elems.len() == 1 {
                value_ast(&t.elems[0], None)
            } else {
                let mut elems = Vec::new();
                for e in &t.elems {
                    elems.push(value_ast(e, None)?);
                }
                Some(json!({ "kind": "tuple", "elems": elems }))
            }
        }
        syn::Expr::Call(c) => {
            if let syn::Expr::Path(p) = &*c.func {
                let name = p.path.segments.last()?.ident.to_string();
                if matches!(name.as_str(), "Some" | "Ok" | "Err") && c.args.len() == 1 {
                    let payload = value_ast(&c.args[0], None)?;
                    return Some(json!({ "kind": "variant", "tag": name, "payload": payload }));
                }
            }
            None
        }
        syn::Expr::Path(p) => match p.path.segments.last()?.ident.to_string().as_str() {
            "None" => Some(json!({ "kind": "variant", "tag": "None" })),
            "true" => Some(json!({ "kind": "bool", "value": true })),
            "false" => Some(json!({ "kind": "bool", "value": false })),
            _ => None,
        },
        syn::Expr::Macro(m) if m.mac.path.is_ident("vec") => {
            let parts = m
                .mac
                .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
                .ok()?;
            let eh = list_elem_hint(hint);
            let mut elems = Vec::new();
            for e in &parts {
                elems.push(value_ast(e, eh)?);
            }
            Some(json!({ "kind": "list", "elems": elems }))
        }
        _ => None,
    }
}

fn doc_text(func: &syn::ItemFn) -> String {
    let mut s = String::new();
    for attr in &func.attrs {
        if attr.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(el) = &nv.value {
                    if let syn::Lit::Str(ls) = &el.lit {
                        s.push_str(&ls.value());
                        s.push('\n');
                    }
                }
            }
        }
    }
    s
}

fn is_call_to(expr: &syn::Expr, fn_name: &str) -> bool {
    match expr {
        // free call `fn_name(args)`
        syn::Expr::Call(c) => matches!(&*c.func, syn::Expr::Path(p)
            if p.path.segments.last().map_or(false, |seg| seg.ident == fn_name)),
        // method call `receiver.fn_name(args)` — the receiver is treated as arg0 (UFCS convention).
        syn::Expr::MethodCall(m) => m.method == fn_name,
        _ => false,
    }
}

/// Positional arguments of a recognised call. For a method call the receiver is prepended as arg0.
fn call_args(expr: &syn::Expr) -> Vec<&syn::Expr> {
    match expr {
        syn::Expr::Call(c) => c.args.iter().collect(),
        syn::Expr::MethodCall(m) => {
            let mut v: Vec<&syn::Expr> = vec![&m.receiver];
            v.extend(m.args.iter());
            v
        }
        _ => Vec::new(),
    }
}

/// Turn `assert_eq!(call, expected)` (either argument order) into a {args, result} example.
fn example_from_assert(
    a: &syn::Expr,
    b: &syn::Expr,
    fn_name: &str,
    param_types: &[Value],
    result_type: Option<&Value>,
) -> Option<Value> {
    let (call, expected) = if is_call_to(a, fn_name) {
        (a, b)
    } else if is_call_to(b, fn_name) {
        (b, a)
    } else {
        return None;
    };
    let mut args = Vec::new();
    for (i, e) in call_args(call).iter().enumerate() {
        args.push(value_ast(e, param_types.get(i))?);
    }
    let result = value_ast(expected, result_type)?;
    Some(json!({ "args": args, "result": result }))
}

/// Extract real examples from the function's `///` doc-tests: parse the fenced code blocks and turn
/// each `assert_eq!(fn_name(literals), literal)` into a value-AST example. No code is executed.
fn doctest_examples(
    func: &syn::ItemFn,
    fn_name: &str,
    param_types: &[Value],
    result_type: Option<&Value>,
) -> Vec<Value> {
    let text = doc_text(func);
    let mut code = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            // Strip hidden doc-test lines (`# ...`), which rustdoc compiles but hides.
            let l = if let Some(rest) = trimmed.strip_prefix("# ") {
                rest
            } else if trimmed == "#" {
                ""
            } else {
                line
            };
            code.push_str(l);
            code.push('\n');
        }
    }
    if code.trim().is_empty() {
        return Vec::new();
    }
    let wrapped = format!("fn __dt() {{\n{}\n}}", code);
    let parsed = match syn::parse_str::<syn::ItemFn>(&wrapped) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for stmt in &parsed.block.stmts {
        let mac = match stmt {
            syn::Stmt::Macro(sm) => &sm.mac,
            syn::Stmt::Expr(syn::Expr::Macro(em), _) => &em.mac,
            _ => continue,
        };
        if !mac.path.is_ident("assert_eq") {
            continue;
        }
        if let Ok(parts) =
            mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            let exprs: Vec<&syn::Expr> = parts.iter().collect();
            if exprs.len() >= 2 {
                if let Some(ex) =
                    example_from_assert(exprs[0], exprs[1], fn_name, param_types, result_type)
                {
                    out.push(ex);
                }
            }
        }
    }
    out
}

// --- v0.2: leading assert! / assert_eq! / assert_ne! as precondition refinements --------------
// Mirrors the Python adapter's `_preconditions` (nl_predicates.predicate_from_py): a leading run of
// `assert*` statements in the body becomes {kind:"pre", expr:<predicate AST>} refinements matching
// predicate-expression.schema.json. Anything whose condition isn't an expressible predicate is
// silently skipped — nothing is fabricated. Rust `&&`/`||`/comparisons are already arity-2 binaries,
// so no chained-comparison folding (Rust forbids `a < b < c`) is needed.

/// A predicate `var`, or None if `name` isn't a valid predicate variable (^[a-z_][a-zA-Z0-9_']*$).
fn p_var(name: &str) -> Option<Value> {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c == '_' || c.is_ascii_lowercase() => {}
        _ => return None,
    }
    if name.len() > 64 || !name.chars().all(|c| c == '_' || c == '\'' || c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(json!({ "kind": "var", "name": name }))
}

fn p_app(op: &str, args: Vec<Value>) -> Value {
    json!({ "kind": "app", "op": op, "args": args })
}

/// A predicate `lit` from a Rust literal. The predicate lit.value is a raw JSON scalar (not a
/// value-AST): bool / number / string. Large ints become decimal strings (matches `int_value`).
fn p_lit_from_lit(lit: &syn::Lit, neg: bool) -> Option<Value> {
    let v = match lit {
        syn::Lit::Bool(b) => json!(b.value),
        syn::Lit::Str(s) => json!(s.value()),
        syn::Lit::Int(i) => {
            let mut n: i64 = i.base10_parse().ok()?;
            if neg {
                n = -n;
            }
            int_value(n)
        }
        syn::Lit::Float(f) => {
            let mut x: f64 = f.base10_parse().ok()?;
            if neg {
                x = -x;
            }
            json!(x)
        }
        _ => return None,
    };
    Some(json!({ "kind": "lit", "value": v }))
}

fn binop_name(op: &syn::BinOp) -> Option<&'static str> {
    use syn::BinOp::*;
    Some(match op {
        And(_) => "and",
        Or(_) => "or",
        Eq(_) => "eq",
        Ne(_) => "neq",
        Lt(_) => "lt",
        Le(_) => "le",
        Gt(_) => "gt",
        Ge(_) => "ge",
        Add(_) => "add",
        Sub(_) => "sub",
        Mul(_) => "mul",
        Div(_) => "div",
        Rem(_) => "mod",
        _ => return None,
    })
}

/// Map a Rust boolean/comparison/arithmetic expression to a predicate AST. None for unsupported forms.
fn predicate_from_expr(expr: &syn::Expr) -> Option<Value> {
    match expr {
        syn::Expr::Paren(p) => predicate_from_expr(&p.expr),
        syn::Expr::Group(g) => predicate_from_expr(&g.expr),
        syn::Expr::Binary(b) => {
            let op = binop_name(&b.op)?;
            Some(p_app(op, vec![predicate_from_expr(&b.left)?, predicate_from_expr(&b.right)?]))
        }
        syn::Expr::Unary(u) => match u.op {
            syn::UnOp::Not(_) => Some(p_app("not", vec![predicate_from_expr(&u.expr)?])),
            syn::UnOp::Neg(_) => {
                // Fold `-<literal>` into the literal; otherwise neg(<expr>).
                if let syn::Expr::Lit(el) = &*u.expr {
                    p_lit_from_lit(&el.lit, true)
                } else {
                    Some(p_app("neg", vec![predicate_from_expr(&u.expr)?]))
                }
            }
            _ => None,
        },
        // `x.len()` -> length(x), the one method call we recognise (mirrors Python's `len(x)`).
        syn::Expr::MethodCall(m) if m.method == "len" && m.args.is_empty() => {
            Some(p_app("length", vec![predicate_from_expr(&m.receiver)?]))
        }
        syn::Expr::Lit(el) => p_lit_from_lit(&el.lit, false),
        syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            p_var(&p.path.segments[0].ident.to_string())
        }
        _ => None,
    }
}

/// Turn one `assert*` macro invocation into a predicate, or None if its condition isn't expressible.
fn assert_macro_predicate(mac: &syn::Macro) -> Option<Value> {
    let name = mac.path.get_ident()?.to_string();
    let parts = mac
        .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    let exprs: Vec<&syn::Expr> = parts.iter().collect();
    match name.as_str() {
        "assert" | "debug_assert" => predicate_from_expr(exprs.first()?),
        "assert_eq" | "debug_assert_eq" => {
            if exprs.len() < 2 {
                return None;
            }
            Some(p_app("eq", vec![predicate_from_expr(exprs[0])?, predicate_from_expr(exprs[1])?]))
        }
        "assert_ne" | "debug_assert_ne" => {
            if exprs.len() < 2 {
                return None;
            }
            Some(p_app("neq", vec![predicate_from_expr(exprs[0])?, predicate_from_expr(exprs[1])?]))
        }
        _ => None,
    }
}

fn is_assert_ident(mac: &syn::Macro) -> bool {
    mac.path
        .get_ident()
        .map(|i| {
            matches!(
                i.to_string().as_str(),
                "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne"
            )
        })
        .unwrap_or(false)
}

/// A leading run of `assert*` statements -> {kind:"pre", expr} refinements. Scanning stops at the
/// first non-assert statement; an assert whose condition isn't expressible is skipped, not a stop.
fn preconditions(func: &syn::ItemFn) -> Vec<Value> {
    let mut refs = Vec::new();
    for stmt in &func.block.stmts {
        let mac = match stmt {
            syn::Stmt::Macro(sm) => &sm.mac,
            syn::Stmt::Expr(syn::Expr::Macro(em), _) => &em.mac,
            _ => break,
        };
        if !is_assert_ident(mac) {
            break;
        }
        if let Some(expr) = assert_macro_predicate(mac) {
            refs.push(json!({ "kind": "pre", "expr": expr }));
        }
    }
    refs
}

// --- v0.2: conservative effect & termination inference (over `syn`) ---------------------------
// A LOWER BOUND: empty `effects` is NOT a purity certificate — it means no syntactically
// recognisable effectful operation was found (indirect/higher-order/dynamic effects are missed).
// `terminates` defaults to "unknown"; "always" only for a call-free, loop-free, non-recursive body;
// "conditional" for direct self-recursion; "never" is never emitted. Mirrors nl_effects.py.

use syn::visit::Visit;

#[derive(Default)]
struct EffectVisitor {
    effects: std::collections::BTreeSet<&'static str>,
}

impl EffectVisitor {
    fn add(&mut self, e: &'static str) {
        self.effects.insert(e);
    }

    fn path_effects(&mut self, path: &syn::Path) {
        let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let last = match segs.last() {
            Some(s) => s.as_str(),
            None => return,
        };
        let has = |n: &str| segs.iter().any(|s| s == n);
        if has("fs") {
            match last {
                "read" | "read_to_string" | "read_to_end" | "read_dir" | "metadata" | "canonicalize" => self.add("fs.read"),
                "write" | "remove_file" | "remove_dir" | "remove_dir_all" | "create_dir"
                | "create_dir_all" | "rename" | "copy" | "hard_link" | "set_permissions" => self.add("fs.write"),
                _ => {}
            }
        }
        if has("File") {
            match last {
                "open" => self.add("fs.read"),
                "create" => self.add("fs.write"),
                _ => {}
            }
        }
        if (has("Instant") || has("SystemTime")) && last == "now" {
            self.add("time");
        }
        if last == "sleep" && (has("thread") || has("time")) {
            self.add("time");
        }
        if has("rand") || last == "thread_rng" || last == "random" {
            self.add("random");
        }
        if has("Command") {
            self.add("process.spawn");
        }
        if has("process") && (last == "exit" || last == "abort") {
            self.add("panic");
        }
        if has("TcpStream") && last == "connect" {
            self.add("net.write");
        }
        if has("reqwest") {
            match last {
                "get" => self.add("net.read"),
                "post" | "put" | "patch" | "delete" => self.add("net.write"),
                _ => {
                    self.add("net.read");
                    self.add("net.write");
                }
            }
        }
        // alloc: explicit heap constructors only (everything else allocates too — no signal).
        if (has("Box") || has("Rc") || has("Arc")) && last == "new" {
            self.add("alloc");
        }
        if has("Vec") && (last == "new" || last == "with_capacity") {
            self.add("alloc");
        }
    }

    fn method_effects(&mut self, name: &str) {
        match name {
            "recv" | "recv_from" => self.add("net.read"),
            "send" | "send_to" => self.add("net.write"),
            "unwrap" | "expect" => self.add("panic"),
            _ => {}
        }
    }

    fn macro_effects(&mut self, name: &str) {
        match name {
            "println" | "print" | "eprintln" | "eprint" => self.add("io.console"),
            "panic" | "unreachable" | "todo" | "unimplemented" => self.add("panic"),
            "vec" | "format" => self.add("alloc"),
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for EffectVisitor {
    fn visit_item_fn(&mut self, _f: &'ast syn::ItemFn) {} // don't descend into nested fn items
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*c.func {
            self.path_effects(&p.path);
        }
        syn::visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        self.method_effects(&m.method.to_string());
        syn::visit::visit_expr_method_call(self, m);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some(id) = mac.path.get_ident() {
            self.macro_effects(&id.to_string());
        }
    }
}

fn infer_effects(func: &syn::ItemFn) -> Vec<&'static str> {
    let mut v = EffectVisitor::default();
    for stmt in &func.block.stmts {
        v.visit_stmt(stmt);
    }
    v.effects.into_iter().collect()
}

#[derive(Default)]
struct TermVisitor<'a> {
    name: &'a str,
    self_rec: bool,
    has_loop: bool,
    has_call: bool,
}

impl<'a, 'ast> Visit<'ast> for TermVisitor<'a> {
    fn visit_item_fn(&mut self, _f: &'ast syn::ItemFn) {}
    fn visit_expr_loop(&mut self, e: &'ast syn::ExprLoop) {
        self.has_loop = true;
        syn::visit::visit_expr_loop(self, e);
    }
    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.has_loop = true;
        syn::visit::visit_expr_while(self, e);
    }
    fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
        self.has_loop = true;
        syn::visit::visit_expr_for_loop(self, e);
    }
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        self.has_call = true;
        if let syn::Expr::Path(p) = &*c.func {
            if p.path.segments.len() == 1 && p.path.segments[0].ident.to_string() == self.name {
                self.self_rec = true;
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        self.has_call = true;
        syn::visit::visit_expr_method_call(self, m);
    }
    fn visit_expr_await(&mut self, a: &'ast syn::ExprAwait) {
        self.has_call = true;
        syn::visit::visit_expr_await(self, a);
    }
}

fn infer_terminates(func: &syn::ItemFn) -> &'static str {
    let name = func.sig.ident.to_string();
    let mut v = TermVisitor { name: &name, ..Default::default() };
    for stmt in &func.block.stmts {
        v.visit_stmt(stmt);
    }
    if v.self_rec {
        "conditional"
    } else if v.has_loop || v.has_call {
        "unknown"
    } else {
        "always"
    }
}

// --- v0.2: pragmatic body-expression AST (spec/body-expression.schema.json) --------------------
// v1 SUBSET: only a single result expression of var/lit/app/field is translated; anything else
// (blocks, let, if/match, method calls, ...) returns None and the caller keeps the synthetic
// source-token hash — byte-identical to before. Parameters are free `var`s (schema-sanctioned).
// Operators map to `app { fn: var(op), args }`, reusing the predicate operator vocabulary.

/// A body-expression `app` over a builtin operator `var`.
fn body_op_app(op: &str, args: Vec<Value>) -> Option<Value> {
    Some(json!({ "kind": "app", "fn": p_var(op)?, "args": args }))
}

fn expr_to_body(expr: &syn::Expr) -> Option<Value> {
    match expr {
        syn::Expr::Paren(p) => expr_to_body(&p.expr),
        syn::Expr::Group(g) => expr_to_body(&g.expr),
        syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            p_var(&p.path.segments[0].ident.to_string())
        }
        syn::Expr::Lit(_) => value_ast(expr, None).map(|v| json!({ "kind": "lit", "value": v })),
        syn::Expr::Unary(u) => match u.op {
            // `-<literal>` folds into the literal; otherwise neg(<expr>) / not(<expr>).
            syn::UnOp::Neg(_) if matches!(&*u.expr, syn::Expr::Lit(_)) => {
                value_ast(expr, None).map(|v| json!({ "kind": "lit", "value": v }))
            }
            syn::UnOp::Neg(_) => body_op_app("neg", vec![expr_to_body(&u.expr)?]),
            syn::UnOp::Not(_) => body_op_app("not", vec![expr_to_body(&u.expr)?]),
            _ => None,
        },
        syn::Expr::Binary(b) => {
            let op = binop_name(&b.op)?;
            body_op_app(op, vec![expr_to_body(&b.left)?, expr_to_body(&b.right)?])
        }
        syn::Expr::Call(c) => {
            let fnv = match &*c.func {
                syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
                    p_var(&p.path.segments[0].ident.to_string())?
                }
                _ => return None,
            };
            let mut args = Vec::new();
            for a in &c.args {
                args.push(expr_to_body(a)?);
            }
            Some(json!({ "kind": "app", "fn": fnv, "args": args }))
        }
        syn::Expr::Field(f) => {
            if let syn::Member::Named(id) = &f.member {
                let name = id.to_string();
                let ok = name.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false)
                    && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
                if !ok {
                    return None;
                }
                Some(json!({ "kind": "field", "record": expr_to_body(&f.base)?, "name": name }))
            } else {
                None // tuple index (`.0`) has no field name
            }
        }
        _ => None,
    }
}

/// A body AST for an expression-bodied fn — a block with a single trailing expression (or a single
/// `return <expr>;`). None for anything else.
fn body_ast(func: &syn::ItemFn) -> Option<Value> {
    let stmts = &func.block.stmts;
    if stmts.len() != 1 {
        return None;
    }
    let expr = match &stmts[0] {
        syn::Stmt::Expr(e, None) => e,
        syn::Stmt::Expr(syn::Expr::Return(r), Some(_)) => r.expr.as_deref()?,
        _ => return None,
    };
    expr_to_body(expr)
}

// --- v0.2: curated algebraic-law catalog (opt-in --properties) ---------------------------------
// The same language-neutral catalog the Python adapters use (ingest-common/property_catalog.json),
// embedded at compile time. Laws are matched by name-hint + arity; attached laws are checkable with
// `nl-validator check-properties`. `properties` is included only when non-empty, so records without
// laws hash exactly as before.

const PROPERTY_CATALOG_JSON: &str = include_str!("../../../ingest-common/property_catalog.json");

fn property_catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(PROPERTY_CATALOG_JSON).expect("property_catalog.json is valid JSON")
    })
}

/// (properties, intent_tags) for catalog laws matching these name_hints + arity + calling convention,
/// de-duplicated by property name / tag value in catalog order. Mirrors property_catalog.py:match_catalog.
/// `is_method` selects the convention: 'function' laws apply to free functions, 'method' laws to
/// methods (receiver = arg0); a law with no convention applies to either.
fn catalog_match(name_hints: &[Value], arity: usize, is_method: bool) -> (Vec<Value>, Vec<Value>) {
    let hints: HashSet<&str> = name_hints.iter().filter_map(|v| v.as_str()).collect();
    let mut props: Vec<Value> = Vec::new();
    let mut prop_names: HashSet<String> = HashSet::new();
    let mut tags: Vec<Value> = Vec::new();
    let laws = match property_catalog().get("laws").and_then(|v| v.as_array()) {
        Some(l) => l,
        None => return (props, tags),
    };
    for law in laws {
        let m = &law["match"];
        let name_ok = m.get("name_hints").and_then(|v| v.as_array()).map_or(false, |hs| {
            hs.iter().any(|h| h.as_str().map_or(false, |s| hints.contains(s)))
        });
        if !name_ok {
            continue;
        }
        if let Some(a) = m.get("arity").and_then(|v| v.as_u64()) {
            if a as usize != arity {
                continue;
            }
        }
        match m.get("convention").and_then(|v| v.as_str()) {
            Some("function") if is_method => continue,
            Some("method") if !is_method => continue,
            _ => {}
        }
        if let Some(prop) = law.get("property") {
            let name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if prop_names.insert(name) {
                props.push(prop.clone());
            }
        }
        if let Some(tag) = law.get("intent_tag").and_then(|v| v.as_str()) {
            let tv = json!(tag);
            if !tags.contains(&tv) {
                tags.push(tv);
            }
        }
    }
    (props, tags)
}

fn build_v2_record(func: &syn::ItemFn, crate_name: Option<&str>, with_properties: bool, is_method: bool) -> Result<Option<Value>> {
    let fn_name = func.sig.ident.to_string();
    let type_ast = type_ast_from_sig(&func.sig);
    let (param_types, result_type) = split_fn_type(&type_ast);
    let examples = doctest_examples(func, &fn_name, &param_types, result_type.as_ref());
    if examples.is_empty() {
        return Ok(None);
    }

    let mut name_hints: Vec<Value> = vec![json!(fn_name)];
    if let Some(krate) = crate_name {
        name_hints.push(json!(format!("{}_{}", krate, fn_name)));
    }
    // Curated algebraic laws for recognised functions (opt-in). Empty unless --properties matches.
    let (catalog_props, catalog_tags) = if with_properties {
        catalog_match(&name_hints, param_types.len(), is_method)
    } else {
        (Vec::new(), Vec::new())
    };
    // A real body-expression AST (canonical JCS hash, a resolvable expr_ address) when the body is
    // in the v1 subset; otherwise the synthetic source-token hash (byte-identical to before).
    let body_hash = match body_ast(func) {
        Some(b) => {
            let canon = canonicalize(&b).context("canonicalising body AST")?;
            format_hash("expr", &blake3_hash(&canon))
        }
        None => {
            let body_tokens = func.block.to_token_stream().to_string();
            format_hash("expr", &blake3_hash(body_tokens.as_bytes()))
        }
    };

    let mut record = json!({
        "schema_version": "0.2.0",
        "hash": "fn_0000000000000000000000000000000000000000000000000000000000000000",
        "name_hints": name_hints,
        "signature": {
            "type": type_ast,
            "refinements": preconditions(func),
            "effects": infer_effects(func),
            "capabilities": [],
            "terminates": infer_terminates(func)
        },
        "examples": examples,
        "intent_tags": catalog_tags,
        "derived_from": null,
        "supersedes": null,
        "body_hash": body_hash
    });
    // `properties` is optional in v0.2; include it only when laws were attached, so records without
    // laws hash exactly as before.
    if !catalog_props.is_empty() {
        record["properties"] = json!(catalog_props);
    }
    record["hash"] = json!(fn_hash(&record)?);
    Ok(Some(record))
}

/// Format a Rust function signature as a Nova Lingua v0.1 type string.
/// Output: `forall T U. (Param1, Param2) -> RetType`  (lifetimes stripped).
fn format_sig(sig: &syn::Signature) -> String {
    // Type-only generic params (lifetimes dropped — irrelevant to the type).
    let type_params: Vec<String> = sig
        .generics
        .params
        .iter()
        .filter_map(|p| {
            if let syn::GenericParam::Type(tp) = p {
                Some(tp.ident.to_string())
            } else {
                None
            }
        })
        .collect();

    let prefix = if type_params.is_empty() {
        String::new()
    } else {
        format!("forall {}. ", type_params.join(" "))
    };

    let params: Vec<String> = sig
        .inputs
        .iter()
        .map(|arg| match arg {
            FnArg::Typed(pt) => tok(&*pt.ty),
            FnArg::Receiver(r) => {
                if r.mutability.is_some() {
                    "&mut Self".into()
                } else if r.reference.is_some() {
                    "&Self".into()
                } else {
                    "Self".into()
                }
            }
        })
        .collect();

    let ret = match &sig.output {
        ReturnType::Default => "Unit".to_string(),
        ReturnType::Type(_, ty) => tok(&**ty),
    };

    format!("{}({}) -> {}", prefix, params.join(", "), ret)
}

/// Render a syn AST node to a token string with minimal whitespace normalisation.
fn tok(node: &impl ToTokens) -> String {
    node.to_token_stream()
        .to_string()
        .replace(" < ", "<")
        .replace(" > ", "> ")
        .replace("> ,", ">,")
        .replace(" , ", ", ")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ::", "::")
        .replace(":: ", "::")
        .trim_end()
        .to_string()
}

/// Compute the fn_ content-address: strip the `hash` field, JCS-canonicalise, BLAKE3-256.
fn fn_hash(record: &Value) -> Result<String> {
    let mut stripped = record.clone();
    stripped
        .as_object_mut()
        .expect("record is always an object")
        .remove("hash");
    let canonical = canonicalize(&stripped).context("JCS canonicalisation failed")?;
    Ok(format_hash("fn", &blake3_hash(&canonical)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn item_fn(src: &str) -> syn::ItemFn {
        syn::parse_str(src).expect("parse fn")
    }

    #[test]
    fn type_ast_maps_primitives_and_containers() {
        let f = item_fn("pub fn f(n: u64, xs: Vec<i32>) -> bool { true }");
        assert_eq!(
            type_ast_from_sig(&f.sig),
            json!({
                "kind": "fn",
                "params": [
                    { "kind": "builtin", "name": "nat" },
                    { "kind": "apply", "ctor": { "kind": "builtin", "name": "List" },
                      "args": [{ "kind": "builtin", "name": "int" }] }
                ],
                "result": { "kind": "builtin", "name": "bool" }
            })
        );
    }

    #[test]
    fn unknown_types_become_quantified_vars() {
        let t = type_ast_from_sig(&item_fn("pub fn id<T>(x: T) -> T { x }").sig);
        assert_eq!(t["kind"], "forall");
        assert_eq!(t["vars"], json!(["a"])); // T used twice -> one variable
        assert_eq!(t["body"]["params"][0], t["body"]["result"]);
    }

    #[test]
    fn no_doctest_is_v1_fallback() {
        let f = item_fn("pub fn noex(x: i32) -> i32 { x }");
        assert!(build_v2_record(&f, None, false, false).unwrap().is_none());
    }

    #[test]
    fn v2_record_from_doctest_validates_and_verifies() {
        let f = item_fn(
            "/// Doubles.\n/// ```\n/// assert_eq!(double(5), 10);\n/// assert_eq!(double(0), 0);\n/// ```\npub fn double(n: u64) -> u64 { n * 2 }",
        );
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        assert_eq!(rec["schema_version"], "0.2.0");
        assert_eq!(rec["examples"][0]["args"][0], json!({ "kind": "nat", "value": 5 }));
        assert_eq!(rec["examples"][0]["result"], json!({ "kind": "nat", "value": 10 }));

        // Hash verifies the way nl-validator computes it.
        let h = nl_validator::hash_artifact_with_kind(&rec, nl_validator::ArtifactKind::FunctionRecord)
            .unwrap();
        assert_eq!(rec["hash"], json!(h));

        // The record validates against the v0.2 schema.
        let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(spec.join("function-record.v0.2.schema.json")).unwrap(),
        )
        .unwrap();
        nl_validator::validate_with_refs(&schema, &rec, &spec).expect("v0.2 record should validate");
    }

    #[test]
    fn leading_asserts_become_preconditions() {
        let f = item_fn(
            "/// ```\n/// assert_eq!(clamp(5), 5);\n/// ```\npub fn clamp(n: i64) -> i64 { assert!(n >= 0); assert!(n < 100); let y = n; y }",
        );
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        let refs = rec["signature"]["refinements"].as_array().unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs[0],
            json!({ "kind": "pre", "expr":
                { "kind": "app", "op": "ge", "args": [
                    { "kind": "var", "name": "n" }, { "kind": "lit", "value": 0 }] } })
        );
        assert_eq!(refs[1]["expr"]["op"], "lt");

        // Refinement preconditions must validate against the v0.2 schema (pe_predicate).
        let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(spec.join("function-record.v0.2.schema.json")).unwrap(),
        )
        .unwrap();
        nl_validator::validate_with_refs(&schema, &rec, &spec).expect("record with preconditions validates");
    }

    #[test]
    fn non_assert_statement_stops_the_precondition_run() {
        // The `let` before the second assert stops scanning — only the first assert is a precondition.
        let f = item_fn(
            "/// ```\n/// assert_eq!(g(2), 2);\n/// ```\npub fn g(n: i64) -> i64 { assert!(n > 0); let _m = n; assert!(n < 9); n }",
        );
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        let refs = rec["signature"]["refinements"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["expr"]["op"], "gt");
    }

    #[test]
    fn inexpressible_assert_is_skipped_not_fatal() {
        // First assert calls an unrecognised function -> skipped; the second (a comparison) survives.
        let f = item_fn(
            "/// ```\n/// assert_eq!(h(3), 3);\n/// ```\npub fn h(n: i64) -> i64 { assert!(valid(n)); assert!(n >= 1); n }",
        );
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        let refs = rec["signature"]["refinements"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["expr"]["op"], "ge");
    }

    #[test]
    fn effects_inferred_from_calls_and_macros() {
        let f = item_fn(
            "pub fn f(p: &str) { let s = std::fs::read_to_string(p).unwrap(); println!(\"{}\", s); }",
        );
        let eff = infer_effects(&f);
        assert!(eff.contains(&"fs.read"));
        assert!(eff.contains(&"io.console"));
        assert!(eff.contains(&"panic")); // .unwrap()
        // sorted (BTreeSet): fs.read < io.console < panic
        assert_eq!(eff, vec!["fs.read", "io.console", "panic"]);
    }

    #[test]
    fn pure_arithmetic_is_effect_free_and_always_terminates() {
        let f = item_fn("pub fn add(a: i64, b: i64) -> i64 { a + b }");
        assert!(infer_effects(&f).is_empty());
        assert_eq!(infer_terminates(&f), "always");
    }

    #[test]
    fn self_recursion_is_conditional_loops_are_unknown() {
        let rec = item_fn("pub fn fact(n: u64) -> u64 { if n == 0 { 1 } else { n * fact(n - 1) } }");
        assert_eq!(infer_terminates(&rec), "conditional");
        let loopy = item_fn("pub fn sum(xs: &[u64]) -> u64 { let mut t = 0; for x in xs { t += x; } t }");
        assert_eq!(infer_terminates(&loopy), "unknown");
        // A call to a non-total helper -> unknown (we can't prove the callee halts).
        let calls = item_fn("pub fn g(n: u64) -> u64 { helper(n) }");
        assert_eq!(infer_terminates(&calls), "unknown");
    }

    #[test]
    fn properties_flag_attaches_catalog_laws() {
        // `reverse/1` matches the catalog: a length-preserving law + the `lossless` intent tag.
        let f = item_fn(
            "/// ```\n/// assert_eq!(reverse(vec![1, 2, 3]), vec![3, 2, 1]);\n/// ```\npub fn reverse(xs: Vec<u64>) -> Vec<u64> { xs }",
        );
        // Without --properties: no properties, no tags (byte-compatible).
        let plain = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        assert!(plain.get("properties").is_none());
        assert_eq!(plain["intent_tags"], json!([]));

        // With --properties: the law and tag are attached.
        let rec = build_v2_record(&f, None, true, false).unwrap().expect("a v0.2 record");
        assert_eq!(rec["properties"][0]["name"], "length_preserving");
        assert_eq!(rec["intent_tags"], json!(["lossless"]));

        // The record still validates and its hash is correct.
        let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(spec.join("function-record.v0.2.schema.json")).unwrap(),
        )
        .unwrap();
        nl_validator::validate_with_refs(&schema, &rec, &spec).expect("record with properties validates");
        let h = nl_validator::hash_artifact_with_kind(&rec, nl_validator::ArtifactKind::FunctionRecord)
            .unwrap();
        assert_eq!(rec["hash"], json!(h));

        // The attached law is CONSISTENT with the worked example (not contradicted).
        assert_eq!(
            nl_validator::evaluate_property(&rec["properties"][0]["expr"],
                rec["examples"].as_array().unwrap()),
            nl_validator::Verdict::Consistent
        );

        // A non-recognised name gets nothing even under --properties.
        let g = item_fn(
            "/// ```\n/// assert_eq!(frob(1), 1);\n/// ```\npub fn frob(n: u64) -> u64 { n }",
        );
        let gr = build_v2_record(&g, None, true, false).unwrap().expect("a v0.2 record");
        assert!(gr.get("properties").is_none());
        assert_eq!(gr["intent_tags"], json!([]));
    }

    fn first_method(src: &str) -> (syn::ImplItemFn, syn::Type, syn::Generics) {
        let imp: syn::ItemImpl = syn::parse_str(src).expect("parse impl");
        let m = imp
            .items
            .iter()
            .find_map(|i| if let syn::ImplItem::Fn(f) = i { Some(f.clone()) } else { None })
            .expect("an impl method");
        (m, (*imp.self_ty).clone(), imp.generics.clone())
    }

    #[test]
    fn impl_method_is_lifted_with_self_as_arg0_and_matches_catalog() {
        // A `reverse` method on a concrete type: the receiver becomes arg0, the method-call doctest
        // mines an example, and the (convention-agnostic) reverse law attaches.
        let (m, self_ty, generics) = first_method(
            "impl<T: Clone> Stack<T> {\n\
             /// ```\n\
             /// assert_eq!(vec![1, 2, 3].reverse(), vec![3, 2, 1]);\n\
             /// ```\n\
             pub fn reverse(&self) -> Vec<T> { unimplemented!() }\n\
             }",
        );
        let func = lift_method(&m.attrs, &m.vis, &m.sig, Some(&m.block), &self_ty, &generics)
            .expect("a method (has a receiver)");
        let rec = build_v2_record(&func, None, true, true).unwrap().expect("a v0.2 record");

        // self is arg0: the example's first arg is the receiver value [1,2,3].
        assert_eq!(rec["examples"][0]["args"][0]["kind"], "list");
        assert_eq!(rec["properties"][0]["name"], "length_preserving");
        assert_eq!(rec["intent_tags"], json!(["lossless"]));
        assert_eq!(
            nl_validator::evaluate_property(&rec["properties"][0]["expr"], rec["examples"].as_array().unwrap()),
            nl_validator::Verdict::Consistent
        );
    }

    #[test]
    fn catalog_distinguishes_function_and_method_conventions() {
        let map = vec![json!("map")];
        // Free `map(f, xs)`: the collection is arg1.
        let (fp, _) = catalog_match(&map, 2, false);
        assert_eq!(fp[0]["expr"]["args"][1]["args"][0]["name"], "arg1");
        // Method `xs.map(f)`: the collection (receiver) is arg0.
        let (mp, _) = catalog_match(&map, 2, true);
        assert_eq!(mp[0]["expr"]["args"][1]["args"][0]["name"], "arg0");
        // An associated fn without a receiver is not a method.
        let (nr, ty, g) = first_method("impl Foo { pub fn new() -> Self { unimplemented!() } }");
        assert!(lift_method(&nr.attrs, &nr.vis, &nr.sig, Some(&nr.block), &ty, &g).is_none());
    }

    #[test]
    fn body_ast_for_expression_body_round_trips() {
        // `n * 2` is a single result expression -> a real body AST: app(var "mul", [var n, lit 2]).
        let f = item_fn(
            "/// ```\n/// assert_eq!(double(5), 10);\n/// ```\npub fn double(n: u64) -> u64 { n * 2 }",
        );
        let b = body_ast(&f).expect("an in-subset body");
        assert_eq!(
            b,
            json!({ "kind": "app", "fn": { "kind": "var", "name": "mul" },
                    "args": [ { "kind": "var", "name": "n" },
                              { "kind": "lit", "value": { "kind": "int", "value": 2 } } ] })
        );

        // The body AST validates against the body-expression schema, and the record's body_hash is
        // exactly the validator's content-address for that body artifact (a resolvable expr_ address).
        let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec");
        let body_schema: Value = serde_json::from_str(
            &std::fs::read_to_string(spec.join("body-expression.schema.json")).unwrap(),
        )
        .unwrap();
        nl_validator::validate_with_refs(&body_schema, &b, &spec).expect("body AST validates");

        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        let body_addr = nl_validator::hash_artifact_with_kind(&b, nl_validator::ArtifactKind::BodyExpression)
            .unwrap();
        assert_eq!(rec["body_hash"], json!(body_addr));
    }

    #[test]
    fn non_subset_body_falls_back_to_synthetic_hash() {
        // A body with a local binding is out of subset -> body_ast None, synthetic hash unchanged.
        let f = item_fn(
            "/// ```\n/// assert_eq!(g(5), 5);\n/// ```\npub fn g(n: u64) -> u64 { let y = n; y }",
        );
        assert!(body_ast(&f).is_none());
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        let body_tokens = f.block.to_token_stream().to_string();
        let synthetic = format_hash("expr", &blake3_hash(body_tokens.as_bytes()));
        assert_eq!(rec["body_hash"], json!(synthetic));
    }

    #[test]
    fn doctest_vec_and_negative_examples() {
        let f = item_fn(
            "/// ```\n/// assert_eq!(rev(vec![1, 2, 3]), vec![3, 2, 1]);\n/// ```\npub fn rev(xs: Vec<u64>) -> Vec<u64> { xs }",
        );
        let rec = build_v2_record(&f, None, false, false).unwrap().expect("a v0.2 record");
        assert_eq!(
            rec["examples"][0]["result"],
            json!({ "kind": "list", "elems": [
                { "kind": "nat", "value": 3 }, { "kind": "nat", "value": 2 }, { "kind": "nat", "value": 1 }] })
        );
    }
}
