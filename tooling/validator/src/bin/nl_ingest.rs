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

    /// Also write a runnable directory: each record as `<fn_hash>.json` and each lifted body as
    /// `<expr_hash>.json`, so `nl-validator run --records <dir>` executes the ingested functions
    /// against their (v2) examples. Bodies outside the executable subset (synthetic-hash fallback)
    /// are not written.
    #[arg(long)]
    emit_dir: Option<PathBuf>,
}

/// Write a record and (if the body lifted to a real AST) its body to `emit_dir`.
fn write_emit(dir: &std::path::Path, record: &Value, func: &syn::ItemFn) -> Result<()> {
    fs::create_dir_all(dir)?;
    let hash = record["hash"].as_str().context("record has no hash")?;
    fs::write(dir.join(format!("{hash}.json")), serde_json::to_string_pretty(record)?)?;
    if let Some(body) = body_ast(func) {
        let addr = record["body_hash"].as_str().context("record has no body_hash")?;
        fs::write(dir.join(format!("{addr}.json")), serde_json::to_string_pretty(&body)?)?;
    }
    Ok(())
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
                    if let Some(dir) = &cli.emit_dir {
                        write_emit(dir, &record, &func)?;
                    }
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
                        if let Some(dir) = &cli.emit_dir {
                            write_emit(dir, &record, &func)?;
                        }
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
                        if let Some(dir) = &cli.emit_dir {
                            write_emit(dir, &record, &func)?;
                        }
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

/// A doc-test evaluation environment: `let`-bound names from the doc-test mapped to their defining
/// expressions, so a variable reference can be resolved without executing anything.
type Env<'a> = HashMap<String, &'a syn::Expr>;

/// Encode a Rust literal expression as a value-expression AST (with an empty environment — used for
/// body literals, where there are no doc-test `let` bindings to resolve).
fn value_ast(expr: &syn::Expr, hint: Option<&Value>) -> Option<Value> {
    eval_value(expr, hint, &Env::new(), 0)
}

/// The doc-test interpreter: evaluate a *value* expression to a value-AST. It is purely structural —
/// it never runs the function under test — but it DOES resolve the value subset of real doc-tests:
/// literals, arrays / `vec!` / tuples, `Some`/`Ok`/`Err`/`None`, references, `let`-bound variables
/// (via `env`), integer ranges (`0..3`, `1..=5`), `.chars()` over a string, and trivial transparent
/// iterator adapters (`.iter()` / `.into_iter()` / `.collect()` / ...). Returns None for anything
/// outside this subset (so the example is skipped — never fabricated). `depth` bounds recursion
/// through `let` chains.
fn eval_value(expr: &syn::Expr, hint: Option<&Value>, env: &Env, depth: usize) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    let d = depth + 1;
    match expr {
        syn::Expr::Lit(el) => lit_value(&el.lit, hint, false),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => match &*u.expr {
            syn::Expr::Lit(el) => lit_value(&el.lit, hint, true),
            _ => None,
        },
        syn::Expr::Group(g) => eval_value(&g.expr, hint, env, d),
        syn::Expr::Paren(p) => eval_value(&p.expr, hint, env, d),
        syn::Expr::Reference(r) => eval_value(&r.expr, hint, env, d), // `&x` / `&[..]` -> x
        syn::Expr::Array(a) => {
            let eh = list_elem_hint(hint);
            let mut elems = Vec::new();
            for e in &a.elems {
                elems.push(eval_value(e, eh, env, d)?);
            }
            Some(json!({ "kind": "list", "elems": elems }))
        }
        syn::Expr::Tuple(t) => {
            if t.elems.is_empty() {
                Some(json!({ "kind": "unit" }))
            } else if t.elems.len() == 1 {
                eval_value(&t.elems[0], None, env, d)
            } else {
                let mut elems = Vec::new();
                for e in &t.elems {
                    elems.push(eval_value(e, None, env, d)?);
                }
                Some(json!({ "kind": "tuple", "elems": elems }))
            }
        }
        syn::Expr::Call(c) => {
            if let syn::Expr::Path(p) = &*c.func {
                let name = p.path.segments.last()?.ident.to_string();
                if matches!(name.as_str(), "Some" | "Ok" | "Err") && c.args.len() == 1 {
                    let payload = eval_value(&c.args[0], None, env, d)?;
                    // Rust's `Some` is Nova's canonical `Maybe` constructor `Just` (map_get /
                    // parse_int / parse_json all produce `Just`/`None`); `Ok`/`Err` are shared.
                    let tag = if name == "Some" { "Just".to_string() } else { name };
                    return Some(json!({ "kind": "variant", "tag": tag, "payload": payload }));
                }
            }
            None
        }
        // Integer range `a..b` / `a..=b` -> the list it enumerates (bounded).
        syn::Expr::Range(r) => range_to_list(r, hint),
        syn::Expr::Path(p) => {
            let name = p.path.get_ident()?.to_string();
            match name.as_str() {
                "None" => return Some(json!({ "kind": "variant", "tag": "None" })),
                "true" => return Some(json!({ "kind": "bool", "value": true })),
                "false" => return Some(json!({ "kind": "bool", "value": false })),
                _ => {}
            }
            // A `let`-bound doc-test variable: evaluate its definition.
            eval_value(env.get(&name)?, hint, env, d)
        }
        syn::Expr::Macro(m) if m.mac.path.is_ident("vec") => {
            let parts = m
                .mac
                .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
                .ok()?;
            let eh = list_elem_hint(hint);
            let mut elems = Vec::new();
            for e in &parts {
                elems.push(eval_value(e, eh, env, d)?);
            }
            Some(json!({ "kind": "list", "elems": elems }))
        }
        // `<string>.chars()` -> a list of single-character strings.
        syn::Expr::MethodCall(m) if m.method == "chars" && m.args.is_empty() => {
            let recv = eval_value(&m.receiver, None, env, d)?;
            let s = recv.get("value").filter(|_| recv["kind"] == "string").and_then(|v| v.as_str())?;
            Some(json!({ "kind": "list",
                "elems": s.chars().map(|c| json!({ "kind": "string", "value": c.to_string() })).collect::<Vec<_>>() }))
        }
        // Trivial iterator/collection adapters that don't change the logical sequence of values
        // (`vec![..].into_iter()`, `xs.iter()`, `.cloned()`, `.collect()`, ...): encode the receiver.
        syn::Expr::MethodCall(m)
            if m.args.is_empty()
                && matches!(
                    m.method.to_string().as_str(),
                    "iter" | "into_iter" | "iter_mut" | "cloned" | "copied" | "to_vec" | "to_owned" | "collect"
                ) =>
        {
            eval_value(&m.receiver, hint, env, d)
        }
        _ => None,
    }
}

/// An `i64` from an integer-literal expression (possibly negated / parenthesised), for range bounds.
fn lit_i64(expr: &syn::Expr) -> Option<i64> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => i.base10_parse().ok(),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => lit_i64(&u.expr).map(|n| -n),
        syn::Expr::Paren(p) => lit_i64(&p.expr),
        syn::Expr::Group(g) => lit_i64(&g.expr),
        _ => None,
    }
}

/// Expand an integer range expression to the value-AST list it enumerates. Both bounds must be
/// integer literals; the span is bounded to keep mining cheap.
fn range_to_list(r: &syn::ExprRange, hint: Option<&Value>) -> Option<Value> {
    let start = lit_i64(r.start.as_deref()?)?;
    let end = lit_i64(r.end.as_deref()?)?;
    let hi = if matches!(r.limits, syn::RangeLimits::Closed(_)) { end.checked_add(1)? } else { end };
    if hi < start || hi - start > 10_000 {
        return None;
    }
    let nat = is_nat_hint(list_elem_hint(hint));
    let elems: Vec<Value> = (start..hi)
        .map(|n| {
            if nat && n >= 0 {
                json!({ "kind": "nat", "value": int_value(n) })
            } else {
                json!({ "kind": "int", "value": int_value(n) })
            }
        })
        .collect();
    Some(json!({ "kind": "list", "elems": elems }))
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

/// Turn `assert_eq!(call, expected)` (either argument order) into a {args, result} example, resolving
/// value expressions against the doc-test `env` (let bindings, ranges, .chars(), ...).
fn example_from_assert(
    a: &syn::Expr,
    b: &syn::Expr,
    fn_name: &str,
    param_types: &[Value],
    result_type: Option<&Value>,
    env: &Env,
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
        args.push(eval_value(e, param_types.get(i), env, 0)?);
    }
    let result = eval_value(expected, result_type, env, 0)?;
    Some(json!({ "args": args, "result": result }))
}

/// The two compared expressions of an `assert_eq!(a, b)` macro invocation.
fn pair_from_assert_eq(mac: &syn::Macro) -> Option<(syn::Expr, syn::Expr)> {
    if !mac.path.is_ident("assert_eq") {
        return None;
    }
    let parts = mac
        .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    let mut it = parts.into_iter();
    Some((it.next()?, it.next()?))
}

/// The two compared expressions of an `assert_equal(a, b)` call — itertools' function that compares
/// two iterables element-wise. Recognised by name (bare or path-qualified), exactly two arguments.
fn pair_from_assert_equal(expr: &syn::Expr) -> Option<(syn::Expr, syn::Expr)> {
    let syn::Expr::Call(c) = expr else { return None };
    let syn::Expr::Path(p) = &*c.func else { return None };
    if p.path.segments.last().map_or(false, |s| s.ident == "assert_equal") && c.args.len() == 2 {
        let mut it = c.args.iter();
        return Some((it.next()?.clone(), it.next()?.clone()));
    }
    None
}

/// Extract real examples from the function's `///` doc-tests: parse the fenced code blocks and turn
/// each `assert_eq!(fn_name(literals), literal)` (or itertools' `assert_equal(...)`) into a value-AST
/// example. No code is executed.
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
    // Collect `let <name> = <expr>;` bindings so variable receivers/args/results resolve.
    let mut env: Env = Env::new();
    for stmt in &parsed.block.stmts {
        if let syn::Stmt::Local(local) = stmt {
            if let syn::Pat::Ident(pi) = &local.pat {
                if let Some(init) = &local.init {
                    env.insert(pi.ident.to_string(), init.expr.as_ref());
                }
            }
        }
    }

    let mut out = Vec::new();
    for stmt in &parsed.block.stmts {
        // The two compared expressions, from `assert_eq!(a, b)` (macro) or `assert_equal(a, b)`
        // (itertools' iterator-equality function — common in iterator-method doc-tests).
        let pair = match stmt {
            syn::Stmt::Macro(sm) => pair_from_assert_eq(&sm.mac),
            syn::Stmt::Expr(syn::Expr::Macro(em), _) => pair_from_assert_eq(&em.mac),
            syn::Stmt::Expr(e, _) => pair_from_assert_equal(e),
            _ => None,
        };
        if let Some((a, b)) = pair {
            if let Some(ex) = example_from_assert(&a, &b, fn_name, param_types, result_type, &env) {
                out.push(ex);
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

/// Whether `expr` is syntactically known to produce a STRING: a `&str`/`String` parameter, a string
/// literal, `x.to_string()` on one, or `+`-concatenation of one. Drives the type-dependent string
/// translations (Rust is typed, but `syn` is not — parameter annotations root the inference, and a
/// wrong guess fails the example gate rather than shipping wrong).
fn is_stringish(expr: &syn::Expr, strs: &HashSet<String>) -> bool {
    match expr {
        syn::Expr::Paren(p) => is_stringish(&p.expr, strs),
        syn::Expr::Group(g) => is_stringish(&g.expr, strs),
        syn::Expr::Reference(r) => is_stringish(&r.expr, strs),
        syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            strs.contains(&p.path.segments[0].ident.to_string())
        }
        syn::Expr::Lit(l) => matches!(l.lit, syn::Lit::Str(_)),
        syn::Expr::MethodCall(m) => m.method == "to_string" && m.args.is_empty(),
        syn::Expr::Binary(b) if matches!(b.op, syn::BinOp::Add(_)) => {
            is_stringish(&b.left, strs) || is_stringish(&b.right, strs)
        }
        _ => false,
    }
}

/// A Rust `match`-arm pattern -> a Nova `case` pattern. `Some`->`Just` (canonical Maybe),
/// `Ok`/`Err` shared; `_`/binders/tuples/literals map across. Ref/mut/rest/or-patterns are out.
fn syn_pat_to_pattern(pat: &syn::Pat) -> Option<Value> {
    match pat {
        syn::Pat::Wild(_) => Some(json!({ "kind": "wildcard" })),
        syn::Pat::Paren(p) => syn_pat_to_pattern(&p.pat),
        syn::Pat::Lit(pl) => {
            let e = syn::Expr::Lit(syn::ExprLit { attrs: vec![], lit: pl.lit.clone() });
            value_ast(&e, None).map(|v| json!({ "kind": "lit", "value": v }))
        }
        // A bare path: `None` -> the nullary Maybe constructor; another uppercase tag -> a nullary
        // variant; a lowercase single ident -> a binder.
        syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
            let name = pi.ident.to_string();
            if name == "None" {
                Some(json!({ "kind": "variant", "tag": "None" }))
            } else if name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                Some(json!({ "kind": "variant", "tag": name }))
            } else {
                p_var(&name).map(|_| json!({ "kind": "bind", "name": name }))
            }
        }
        syn::Pat::Path(p) => {
            let name = p.path.segments.last()?.ident.to_string();
            if name == "None" {
                Some(json!({ "kind": "variant", "tag": "None" }))
            } else if name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                Some(json!({ "kind": "variant", "tag": name }))
            } else {
                None
            }
        }
        // `Some(p)` / `Ok(p)` / `Err(p)` -> a variant pattern with an inner payload pattern.
        syn::Pat::TupleStruct(ts) => {
            let name = ts.path.segments.last()?.ident.to_string();
            if ts.elems.len() != 1 {
                return None;
            }
            let tag = if name == "Some" { "Just".to_string() } else { name };
            let payload = syn_pat_to_pattern(ts.elems.first()?)?;
            Some(json!({ "kind": "variant", "tag": tag, "payload": payload }))
        }
        // `(a, b)` -> a tuple pattern (>=2 elements).
        syn::Pat::Tuple(pt) => {
            if pt.elems.len() < 2 {
                return None;
            }
            let mut elems = Vec::new();
            for e in &pt.elems {
                elems.push(syn_pat_to_pattern(e)?);
            }
            Some(json!({ "kind": "tuple", "elems": elems }))
        }
        _ => None,
    }
}

/// A Rust closure `|x| body` / `|&x| body` -> a Nova `lambda` over its (simple-ident) parameters.
/// Reference patterns (`&x`) bind the plain name (Nova has no references). Typed / destructuring /
/// multi-statement-block closures are out of subset.
fn closure_to_lambda(c: &syn::ExprClosure, strs: &HashSet<String>) -> Option<Value> {
    let mut params = Vec::new();
    for p in &c.inputs {
        let ident = match p {
            syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => pi.ident.to_string(),
            syn::Pat::Reference(r) => match &*r.pat {
                syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => pi.ident.to_string(),
                _ => return None,
            },
            _ => return None,
        };
        if p_var(&ident).is_none() {
            return None;
        }
        params.push(json!({ "name": ident }));
    }
    if params.is_empty() {
        return None;
    }
    let body = expr_to_body(&c.body, strs)?;
    Some(json!({ "kind": "lambda", "params": params, "body": body }))
}

fn expr_to_body(expr: &syn::Expr, strs: &HashSet<String>) -> Option<Value> {
    match expr {
        syn::Expr::Paren(p) => expr_to_body(&p.expr, strs),
        syn::Expr::Group(g) => expr_to_body(&g.expr, strs),
        syn::Expr::Reference(r) => expr_to_body(&r.expr, strs),
        syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            // `None` is the nullary Maybe constructor (Nova's canonical Maybe is `Just`/`None`);
            // any other single-segment path is a variable.
            if p.path.segments[0].ident == "None" {
                Some(json!({ "kind": "variant", "tag": "None" }))
            } else {
                p_var(&p.path.segments[0].ident.to_string())
            }
        }
        syn::Expr::Lit(_) => value_ast(expr, None).map(|v| json!({ "kind": "lit", "value": v })),
        syn::Expr::Unary(u) => match u.op {
            // `-<literal>` folds into the literal; otherwise neg(<expr>) / not(<expr>).
            syn::UnOp::Neg(_) if matches!(&*u.expr, syn::Expr::Lit(_)) => {
                value_ast(expr, None).map(|v| json!({ "kind": "lit", "value": v }))
            }
            syn::UnOp::Neg(_) => body_op_app("neg", vec![expr_to_body(&u.expr, strs)?]),
            syn::UnOp::Not(_) => body_op_app("not", vec![expr_to_body(&u.expr, strs)?]),
            // `*x` (deref, common in iterator closures over `&T`) is transparent: the value IS the
            // element (Nova has no references).
            syn::UnOp::Deref(_) => expr_to_body(&u.expr, strs),
            _ => None,
        },
        syn::Expr::Binary(b) => {
            // `+` over a known string is str_concat (spec/expressiveness.md phase 4).
            if matches!(b.op, syn::BinOp::Add(_))
                && (is_stringish(&b.left, strs) || is_stringish(&b.right, strs))
            {
                return body_op_app(
                    "str_concat",
                    vec![expr_to_body(&b.left, strs)?, expr_to_body(&b.right, strs)?],
                );
            }
            let op = binop_name(&b.op)?;
            body_op_app(op, vec![expr_to_body(&b.left, strs)?, expr_to_body(&b.right, strs)?])
        }
        // String-method idioms on a KNOWN string receiver. NB `s.len()` counts BYTES in Rust and
        // Unicode scalars in Nova Lingua — identical on ASCII; a non-ASCII doc-test example fails
        // the gate rather than shipping a wrong equivalence. `format!` (a macro) and the
        // iterator-returning `.split()` stay out of subset.
        syn::Expr::MethodCall(m) if is_stringish(&m.receiver, strs) => {
            let recv = expr_to_body(&m.receiver, strs)?;
            match (m.method.to_string().as_str(), m.args.len()) {
                ("len", 0) => body_op_app("str_length", vec![recv]),
                ("is_empty", 0) => body_op_app(
                    "eq",
                    vec![body_op_app("str_length", vec![recv])?, json!({ "kind": "lit", "value": { "kind": "int", "value": 0 } })],
                ),
                ("contains", 1) => body_op_app(
                    "str_contains",
                    vec![expr_to_body(m.args.first()?, strs)?, recv],
                ),
                ("to_string", 0) => Some(recv), // a string's to_string is the identity
                _ => None,
            }
        }
        // `n.to_string()` on a non-string is the canonical decimal rendering.
        syn::Expr::MethodCall(m) if m.method == "to_string" && m.args.is_empty() => {
            body_op_app("to_string", vec![expr_to_body(&m.receiver, strs)?])
        }
        // Iterator-method chains — the idiomatic Rust way to work a collection, mapping directly
        // onto Nova's map/filter/foldl. Adapter methods (`iter`/`into_iter`/`cloned`/`copied`/
        // `collect`/`by_ref`) are transparent (they don't change the value in this pure model).
        syn::Expr::MethodCall(m) => {
            let method = m.method.to_string();
            let n = m.args.len();
            match (method.as_str(), n) {
                ("iter" | "into_iter" | "cloned" | "copied" | "collect" | "by_ref", 0) => {
                    expr_to_body(&m.receiver, strs) // transparent
                }
                ("rev", 0) => body_op_app("reverse", vec![expr_to_body(&m.receiver, strs)?]),
                ("count" | "len", 0) => body_op_app("length", vec![expr_to_body(&m.receiver, strs)?]),
                // `.sum()` / `.product()` fold the numeric identity over the list.
                ("sum", 0) => body_op_app("foldl", vec![
                    p_var("add")?, json!({ "kind": "lit", "value": { "kind": "int", "value": 0 } }),
                    expr_to_body(&m.receiver, strs)?]),
                ("product", 0) => body_op_app("foldl", vec![
                    p_var("mul")?, json!({ "kind": "lit", "value": { "kind": "int", "value": 1 } }),
                    expr_to_body(&m.receiver, strs)?]),
                ("map", 1) | ("filter", 1) => {
                    let syn::Expr::Closure(c) = m.args.first()? else { return None };
                    let lam = closure_to_lambda(c, strs)?;
                    body_op_app(&method, vec![lam, expr_to_body(&m.receiver, strs)?])
                }
                // `.fold(init, |acc, x| …)` -> `foldl(\acc x -> …, init, recv)`.
                ("fold", 2) => {
                    let init = expr_to_body(&m.args[0], strs)?;
                    let syn::Expr::Closure(c) = &m.args[1] else { return None };
                    let lam = closure_to_lambda(c, strs)?;
                    body_op_app("foldl", vec![lam, init, expr_to_body(&m.receiver, strs)?])
                }
                _ => None,
            }
        }
        // `if c { a } else { b }` -> `case c of { true => a; false => b }`. Rust conditions are
        // statically `bool` (no truthiness), so any if-cond is a safe boolean scrutinee. An `if`
        // without an `else` doesn't produce a value in every branch, so it stays out of subset.
        syn::Expr::If(e) => {
            let else_branch = e.else_branch.as_ref()?;
            let cond = expr_to_body(&e.cond, strs)?;
            let then = block_to_body(&e.then_branch.stmts, strs)?;
            let els = expr_to_body(&else_branch.1, strs)?;
            Some(json!({ "kind": "case", "scrutinee": cond, "arms": [
                { "pattern": { "kind": "lit", "value": { "kind": "bool", "value": true } }, "body": then },
                { "pattern": { "kind": "wildcard" }, "body": els }] }))
        }
        // A block `{ … }` in expression position (e.g. an `else { let y = …; y }` branch) is its
        // statement sequence translated as a value.
        syn::Expr::Block(b) => block_to_body(&b.block.stmts, strs),
        // `match s { pat => body, … }` -> `case s of { <pat> => <body>; … }`. Arm guards
        // (`pat if cond =>`) and or-patterns (`a | b =>`) stay out of subset.
        syn::Expr::Match(m) => {
            let scrutinee = expr_to_body(&m.expr, strs)?;
            let mut arms = Vec::new();
            for arm in &m.arms {
                if arm.guard.is_some() {
                    return None;
                }
                let pattern = syn_pat_to_pattern(&arm.pat)?;
                let body = expr_to_body(&arm.body, strs)?;
                arms.push(json!({ "pattern": pattern, "body": body }));
            }
            if arms.is_empty() {
                return None;
            }
            Some(json!({ "kind": "case", "scrutinee": scrutinee, "arms": arms }))
        }
        // Tuple construction `(a, b, …)` -> the `tuple` body node (>=2 elements; a 1-tuple is its
        // element, `()` is unit — matching the value layer and the Python adapter).
        syn::Expr::Tuple(t) => {
            if t.elems.is_empty() {
                return Some(json!({ "kind": "lit", "value": { "kind": "unit" } }));
            }
            if t.elems.len() == 1 {
                return expr_to_body(&t.elems[0], strs);
            }
            let mut elems = Vec::new();
            for e in &t.elems {
                elems.push(expr_to_body(e, strs)?);
            }
            Some(json!({ "kind": "tuple", "elems": elems }))
        }
        syn::Expr::Call(c) => {
            // Variant construction with a computed payload: Rust `Some(e)` -> `Just(e)` (canonical
            // Maybe), `Ok(e)`/`Err(e)` shared with Nova's Result.
            if let syn::Expr::Path(p) = &*c.func {
                if p.qself.is_none() {
                    let name = p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                    if matches!(name.as_str(), "Some" | "Ok" | "Err") && c.args.len() == 1 {
                        let tag = if name == "Some" { "Just" } else { name.as_str() };
                        return Some(json!({ "kind": "variant", "tag": tag,
                                            "payload": expr_to_body(&c.args[0], strs)? }));
                    }
                }
            }
            let fnv = match &*c.func {
                syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
                    p_var(&p.path.segments[0].ident.to_string())?
                }
                _ => return None,
            };
            let mut args = Vec::new();
            for a in &c.args {
                args.push(expr_to_body(a, strs)?);
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
                Some(json!({ "kind": "field", "record": expr_to_body(&f.base, strs)?, "name": name }))
            } else {
                None // tuple index (`.0`) has no field name
            }
        }
        _ => None,
    }
}

/// The parameter names typed `&str` / `String` — the roots of the known-string inference.
fn str_typed_params(func: &syn::ItemFn) -> HashSet<String> {
    let mut out = HashSet::new();
    for input in &func.sig.inputs {
        if let syn::FnArg::Typed(pt) = input {
            if let syn::Pat::Ident(pi) = &*pt.pat {
                let is_str = match &*pt.ty {
                    syn::Type::Reference(r) => {
                        matches!(&*r.elem, syn::Type::Path(p) if p.path.is_ident("str") || p.path.is_ident("String"))
                    }
                    syn::Type::Path(p) => p.path.is_ident("String") || p.path.is_ident("str"),
                    _ => false,
                };
                if is_str {
                    out.insert(pi.ident.to_string());
                }
            }
        }
    }
    out
}

/// Translate a statement sequence that must produce a value into a body expression: a `let PAT = e;`
/// becomes a `let` binding over the rest (a tuple pattern destructures via a one-arm `case`), and the
/// final statement (a trailing expression, or a `return <expr>;`) is the result. `None` for anything
/// outside the subset. Mirrors the Python adapter's `_block_from_py`.
fn block_to_body(stmts: &[syn::Stmt], strs: &HashSet<String>) -> Option<Value> {
    let (head, tail) = stmts.split_first()?;
    match head {
        // The result position: a trailing expression, or `return <expr>;` (statements after a
        // return/trailing expr are dead).
        syn::Stmt::Expr(syn::Expr::Return(r), _) => expr_to_body(r.expr.as_deref()?, strs),
        // An accumulator `for` loop (handled BEFORE the generic trailing-expr arm, since a
        // semicolon-less loop parses as `Stmt::Expr(ForLoop, None)`).
        syn::Stmt::Expr(syn::Expr::ForLoop(fl), _) => {
            let syn::Pat::Ident(pi) = &*fl.pat else { return None };
            if pi.by_ref.is_some() || pi.subpat.is_some() {
                return None;
            }
            let x = pi.ident.to_string();
            if p_var(&x).is_none() {
                return None;
            }
            let src = expr_to_body(&fl.expr, strs)?;
            if fl.body.stmts.len() != 1 {
                return None;
            }
            let loop_strs: HashSet<String> = strs.iter().filter(|s| **s != x).cloned().collect();
            let (acc, update) = accumulator_update(&fl.body.stmts[0], &loop_strs)?;
            let rest = block_to_body(tail, &(strs - &HashSet::from([acc.clone()])))?;
            let lam = json!({ "kind": "lambda",
                "params": [{ "name": acc }, { "name": x }], "body": update });
            let fold = json!({ "kind": "app", "fn": p_var("foldl")?,
                "args": [lam, { "kind": "var", "name": acc }, src] });
            Some(json!({ "kind": "let", "name": acc, "value": fold, "body": rest }))
        }
        syn::Stmt::Expr(e, None) => expr_to_body(e, strs),
        // `let x = e;` -> `let x = <e> in <rest>`. Only a plain single-name binding with an
        // initializer (no `let x;`, no `let-else`, no type-only) is in subset here; a tuple pattern
        // binding is handled below.
        syn::Stmt::Local(local) => {
            let init = local.init.as_ref()?;
            if init.diverge.is_some() {
                return None; // `let … else { … }`
            }
            let value = expr_to_body(&init.expr, strs)?;
            // `let x: T = e` (Pat::Type) unwraps to its inner pattern — the annotation is redundant
            // for the untyped body AST (types are checked against the record signature).
            let pat = match &local.pat {
                syn::Pat::Type(pt) => &*pt.pat,
                other => other,
            };
            match pat {
                syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
                    let name = pi.ident.to_string();
                    if p_var(&name).is_none() {
                        return None;
                    }
                    let inner_strs = if is_stringish(&init.expr, strs) {
                        let mut s = strs.clone();
                        s.insert(name.clone());
                        s
                    } else {
                        let mut s = strs.clone();
                        s.remove(&name);
                        s
                    };
                    let rest = block_to_body(tail, &inner_strs)?;
                    Some(json!({ "kind": "let", "name": name, "value": value, "body": rest }))
                }
                // `let (x, y) = e;` -> `case e of { (x, y) => <rest> }` — tuple-unpacking binding.
                syn::Pat::Tuple(pt) => {
                    let mut binds = Vec::new();
                    for p in &pt.elems {
                        match p {
                            syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none()
                                && p_var(&pi.ident.to_string()).is_some() =>
                            {
                                binds.push(json!({ "kind": "bind", "name": pi.ident.to_string() }));
                            }
                            _ => return None,
                        }
                    }
                    if binds.len() < 2 {
                        return None;
                    }
                    let rest = block_to_body(tail, strs)?;
                    Some(json!({ "kind": "case", "scrutinee": value, "arms": [
                        { "pattern": { "kind": "tuple", "elems": binds }, "body": rest }] }))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A single accumulator statement inside a `for` loop body: `acc <op>= e` or `acc = update`, over an
/// accumulator that must be a plain name. Returns `(acc_name, update_expr)`.
fn accumulator_update(stmt: &syn::Stmt, strs: &HashSet<String>) -> Option<(String, Value)> {
    let expr = match stmt {
        syn::Stmt::Expr(e, _) => e,
        _ => return None,
    };
    match expr {
        // Plain `acc = update`.
        syn::Expr::Assign(a) => {
            let acc = path_ident(&a.left)?;
            Some((acc, expr_to_body(&a.right, strs)?))
        }
        // Compound `acc += e` / `-=` / `*=` / `/=` / `%=` (syn 2.0 models these as `Expr::Binary`).
        syn::Expr::Binary(b) => {
            let op = match b.op {
                syn::BinOp::AddAssign(_) => "add",
                syn::BinOp::SubAssign(_) => "sub",
                syn::BinOp::MulAssign(_) => "mul",
                syn::BinOp::DivAssign(_) => "div",
                syn::BinOp::RemAssign(_) => "mod",
                _ => return None,
            };
            let acc = path_ident(&b.left)?;
            // String `s += t` over a known string is concatenation, like binary `+`.
            let op = if op == "add" && (strs.contains(&acc) || is_stringish(&b.right, strs)) {
                "str_concat"
            } else {
                op
            };
            let update = body_op_app(op, vec![json!({ "kind": "var", "name": acc }),
                                              expr_to_body(&b.right, strs)?])?;
            Some((acc, update))
        }
        _ => None,
    }
}

/// The single-segment identifier of a path expression (`acc`), or None.
fn path_ident(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            Some(p.path.segments[0].ident.to_string())
        }
        _ => None,
    }
}

/// The parameter names of a function, in order — each a plain ident. `None` if any parameter is
/// destructured / a receiver (not wrappable as a lambda binder).
fn param_names(func: &syn::ItemFn) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for arg in &func.sig.inputs {
        match arg {
            syn::FnArg::Typed(pt) => match &*pt.pat {
                syn::Pat::Ident(pi) if pi.by_ref.is_none() && pi.subpat.is_none() => {
                    let n = pi.ident.to_string();
                    p_var(&n)?; // a valid binder name
                    out.push(n);
                }
                _ => return None,
            },
            syn::FnArg::Receiver(_) => return None, // a raw receiver (lift_method rewrites these)
        }
    }
    Some(out)
}

/// A body AST for a function whose block is in the statement subset (`let` bindings, `if`/`match`,
/// iterator chains / accumulator loops, tuple/variant construction, and a value-producing tail).
/// The result is a **`lambda`** over the parameters — the canonical *runnable* form (matching the
/// Python adapter), so `nl-validator run`/`certify` can apply the mined examples. A 0-parameter
/// function is its bare result (applying it to `[]` still evaluates). None if outside the subset.
fn body_ast(func: &syn::ItemFn) -> Option<Value> {
    let inner = block_to_body(&func.block.stmts, &str_typed_params(func))?;
    let names = param_names(func)?;
    if names.is_empty() {
        Some(inner)
    } else {
        Some(json!({ "kind": "lambda",
            "params": names.iter().map(|n| json!({ "name": n })).collect::<Vec<_>>(),
            "body": inner }))
    }
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
    fn assert_equal_doctest_mines_examples_for_iterator_methods() {
        // itertools-style trait method whose doc-test uses `assert_equal` over iterator expressions.
        // The trivial `.into_iter()` adapter is transparent, so the literal receiver encodes.
        let (m, self_ty, generics) = first_method(
            "impl Helpers {\n\
             /// ```\n\
             /// itertools::assert_equal(vec![3, 1, 2].into_iter().sorted(), vec![1, 2, 3]);\n\
             /// ```\n\
             pub fn sorted(self) -> Vec<u64> { unimplemented!() }\n\
             }",
        );
        let func = lift_method(&m.attrs, &m.vis, &m.sig, Some(&m.block), &self_ty, &generics).unwrap();
        let rec = build_v2_record(&func, None, true, true).unwrap().expect("a v0.2 record");
        // The receiver `vec![3,1,2].into_iter()` is mined as arg0 = [3,1,2].
        assert_eq!(
            rec["examples"][0]["args"][0],
            json!({ "kind": "list", "elems": [
                { "kind": "int", "value": 3 }, { "kind": "int", "value": 1 }, { "kind": "int", "value": 2 }] })
        );
        assert_eq!(rec["properties"][0]["name"], "length_preserving");
        assert_eq!(
            nl_validator::evaluate_property(&rec["properties"][0]["expr"], rec["examples"].as_array().unwrap()),
            nl_validator::Verdict::Consistent
        );
    }

    #[test]
    fn interpreter_resolves_let_bindings_chars_and_ranges() {
        // The exact shape of itertools' `sorted` doc-test: a let-bound string + `.chars()`.
        let (m, self_ty, generics) = first_method(
            "impl Helpers {\n\
             /// ```\n\
             /// let text = \"bdacfe\";\n\
             /// itertools::assert_equal(text.chars().sorted(), \"abcdef\".chars());\n\
             /// ```\n\
             pub fn sorted(self) -> Vec<char> { unimplemented!() }\n\
             }",
        );
        let func = lift_method(&m.attrs, &m.vis, &m.sig, Some(&m.block), &self_ty, &generics).unwrap();
        let rec = build_v2_record(&func, None, true, true).unwrap().expect("a v0.2 record");
        // `text.chars()` resolved through the let binding to a 6-element list of single-char strings.
        assert_eq!(rec["examples"][0]["args"][0]["elems"].as_array().unwrap().len(), 6);
        assert_eq!(rec["examples"][0]["args"][0]["elems"][0], json!({ "kind": "string", "value": "b" }));
        assert_eq!(rec["examples"][0]["result"]["elems"][0], json!({ "kind": "string", "value": "a" }));
        assert_eq!(rec["properties"][0]["name"], "length_preserving");
        assert_eq!(
            nl_validator::evaluate_property(&rec["properties"][0]["expr"], rec["examples"].as_array().unwrap()),
            nl_validator::Verdict::Consistent
        );

        // Ranges expand: `(0..3)` -> [0, 1, 2].
        let range: syn::Expr = syn::parse_str("0..3").unwrap();
        assert_eq!(
            value_ast(&range, None).unwrap(),
            json!({ "kind": "list", "elems": [
                { "kind": "int", "value": 0 }, { "kind": "int", "value": 1 }, { "kind": "int", "value": 2 }] })
        );
    }

    #[test]
    fn body_ast_for_expression_body_round_trips() {
        // `n * 2` -> a body lambda-wrapped over the parameters (the runnable form):
        // \n -> mul(n, 2).
        let f = item_fn(
            "/// ```\n/// assert_eq!(double(5), 10);\n/// ```\npub fn double(n: u64) -> u64 { n * 2 }",
        );
        let b = body_ast(&f).expect("an in-subset body");
        assert_eq!(
            b,
            json!({ "kind": "lambda", "params": [ { "name": "n" } ], "body":
                { "kind": "app", "fn": { "kind": "var", "name": "mul" },
                  "args": [ { "kind": "var", "name": "n" },
                            { "kind": "lit", "value": { "kind": "int", "value": 2 } } ] } })
        );
        // And it now EXECUTES: double(5) = 10.
        assert_eq!(nl_validator::eval_body(&b, &[json!({ "kind": "nat", "value": 5 })]).unwrap(),
                   json!({ "kind": "int", "value": 10 }));

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
        // A body still outside the subset (a `while` loop) -> body_ast None, synthetic hash
        // unchanged. (`let y = n; y` used to be the example here, but `let` bindings are now in
        // subset — see `body_if_and_let_lift_and_execute`.)
        let f = item_fn(
            "/// ```\n/// assert_eq!(g(5), 0);\n/// ```\npub fn g(n: u64) -> u64 { let mut x = n; while x > 0 { x -= 1; } x }",
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

    #[test]
    fn body_tuple_and_variant_construction() {
        // Tuple construction in an expression body.
        let t: syn::Expr = syn::parse_str("(a + b, a - b)").unwrap();
        assert_eq!(
            expr_to_body(&t, &HashSet::new()).unwrap(),
            json!({ "kind": "tuple", "elems": [
                { "kind": "app", "fn": { "kind": "var", "name": "add" },
                  "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] },
                { "kind": "app", "fn": { "kind": "var", "name": "sub" },
                  "args": [{ "kind": "var", "name": "a" }, { "kind": "var", "name": "b" }] }] })
        );
        // Rust `Some(e)` -> Nova's canonical `Just(e)`; `None` -> the nullary variant.
        let some: syn::Expr = syn::parse_str("Some(n + 1)").unwrap();
        assert_eq!(
            expr_to_body(&some, &HashSet::new()).unwrap(),
            json!({ "kind": "variant", "tag": "Just",
                    "payload": { "kind": "app", "fn": { "kind": "var", "name": "add" },
                                 "args": [{ "kind": "var", "name": "n" },
                                          { "kind": "lit", "value": { "kind": "int", "value": 1 } }] } })
        );
        let none: syn::Expr = syn::parse_str("None").unwrap();
        assert_eq!(expr_to_body(&none, &HashSet::new()).unwrap(),
                   json!({ "kind": "variant", "tag": "None" }));
        // `Ok`/`Err` keep their tags (Result is shared with Rust).
        let ok: syn::Expr = syn::parse_str("Ok(x)").unwrap();
        assert_eq!(expr_to_body(&ok, &HashSet::new()).unwrap(),
                   json!({ "kind": "variant", "tag": "Ok", "payload": { "kind": "var", "name": "x" } }));
    }

    #[test]
    fn body_if_and_let_lift_and_execute() {
        // if/else-if/else -> nested case; let -> let binding. body_ast lambda-wraps over the
        // parameters, so it is directly runnable — EXECUTE it against the doctest values.
        let int = |n: i64| json!({ "kind": "int", "value": n });
        let run = |src: &str, cases: &[(Vec<i64>, i64)]| {
            let f = item_fn(src);
            let lam = body_ast(&f).expect("an in-subset statement body");
            for (args, want) in cases {
                let av: Vec<Value> = args.iter().map(|n| int(*n)).collect();
                assert_eq!(nl_validator::eval_body(&lam, &av).unwrap(), int(*want),
                           "{src} on {args:?}");
            }
        };
        run("pub fn sign(n: i64) -> i64 { if n > 0 { 1 } else if n < 0 { -1 } else { 0 } }",
            &[(vec![5], 1), (vec![-3], -1), (vec![0], 0)]);
        run("pub fn abs_diff(a: i64, b: i64) -> i64 { let d = a - b; if d < 0 { -d } else { d } }",
            &[(vec![3, 7], 4), (vec![7, 3], 4)]);
        // An annotated `let x: T = e` binding (Pat::Type) unwraps to the same binding.
        run("pub fn twice_plus(n: i64) -> i64 { let d: i64 = n + n; d + 1 }",
            &[(vec![3], 7), (vec![0], 1)]);
        // A tuple-unpacking `let` destructures via a one-arm case.
        run("pub fn f(a: i64, b: i64) -> i64 { let (x, y) = (b, a); x - y }",
            &[(vec![3, 10], 7), (vec![10, 3], -7)]);
        // An `if` with no `else` does not produce a value in every branch -> out of subset.
        let noelse = item_fn("pub fn g(n: i64) -> i64 { if n > 0 { return 1; } 0 }");
        assert!(body_ast(&noelse).is_none());
    }

    #[test]
    fn body_match_lifts_and_executes() {
        let int = |n: i64| json!({ "kind": "int", "value": n });
        // A match on an Option: Some(x) => x ; None => d — the Just/None reader side.
        let f = item_fn("pub fn unwrap_or(o: Option<i64>, d: i64) -> i64 { match o { Some(x) => x, None => d } }");
        let lam = body_ast(&f).expect("match body in subset");
        assert_eq!(lam["body"]["kind"], "case"); // lambda over the case
        assert_eq!(lam["body"]["arms"][0]["pattern"]["tag"], "Just"); // Some -> Just
        let some7 = json!({ "kind": "variant", "tag": "Just", "payload": int(7) });
        let none = json!({ "kind": "variant", "tag": "None" });
        assert_eq!(nl_validator::eval_body(&lam, &[some7, int(0)]).unwrap(), int(7));
        assert_eq!(nl_validator::eval_body(&lam, &[none, int(0)]).unwrap(), int(0));
        // A match on a literal with a wildcard default.
        let g = item_fn("pub fn label(n: i64) -> i64 { match n { 0 => 0, _ => 1 } }");
        let lam2 = body_ast(&g).unwrap();
        assert_eq!(nl_validator::eval_body(&lam2, &[int(0)]).unwrap(), int(0));
        assert_eq!(nl_validator::eval_body(&lam2, &[int(4)]).unwrap(), int(1));
        // A guarded arm is out of subset.
        let guarded = item_fn("pub fn h(n: i64) -> i64 { match n { x if x > 0 => 1, _ => 0 } }");
        assert!(body_ast(&guarded).is_none());
    }

    #[test]
    fn body_iterator_chains_lift_and_execute() {
        let int = |n: i64| json!({ "kind": "int", "value": n });
        let list = |ns: &[i64]| json!({ "kind": "list", "elems": ns.iter().map(|n| int(*n)).collect::<Vec<_>>() });
        // map: xs.iter().map(|x| x * 2).collect() -> map(\x -> x*2, xs). body_ast lambda-wraps.
        let dbl = body_ast(&item_fn("pub fn dbl(xs: Vec<i64>) -> Vec<i64> { xs.iter().map(|x| x * 2).collect() }")).unwrap();
        assert_eq!(nl_validator::eval_body(&dbl, &[list(&[1, 2, 3])]).unwrap(), list(&[2, 4, 6]));
        // filter + count: xs.iter().filter(|&x| x > 0).count() -> length(filter(\x -> x>0, xs)).
        let npos = body_ast(&item_fn("pub fn npos(xs: Vec<i64>) -> usize { xs.iter().filter(|&x| x > 0).count() }")).unwrap();
        assert_eq!(nl_validator::eval_body(&npos, &[list(&[1, -2, 3, -4])]).unwrap(), int(2));
        // sum: xs.iter().sum() -> foldl add 0 xs.
        let tot = body_ast(&item_fn("pub fn tot(xs: Vec<i64>) -> i64 { xs.iter().sum() }")).unwrap();
        assert_eq!(nl_validator::eval_body(&tot, &[list(&[3, 4, 5])]).unwrap(), int(12));
        // fold with an explicit accumulator closure.
        let tot2 = body_ast(&item_fn("pub fn tot2(xs: Vec<i64>) -> i64 { xs.iter().fold(0, |acc, x| acc + x) }")).unwrap();
        assert_eq!(nl_validator::eval_body(&tot2, &[list(&[3, 4, 5])]).unwrap(), int(12));
        // rev.
        let rv = body_ast(&item_fn("pub fn rv(xs: Vec<i64>) -> Vec<i64> { xs.iter().rev().collect() }")).unwrap();
        assert_eq!(nl_validator::eval_body(&rv, &[list(&[1, 2, 3])]).unwrap(), list(&[3, 2, 1]));
    }

    #[test]
    fn executable_corpus_runs() {
        // End to end: ingest a real-shaped Rust module (fixtures/sample_ingest.rs) spanning the
        // whole executable subset, and RUN each function's doctest-mined examples in-process —
        // ingest -> lift (lambda-wrapped body) -> eval == the mined result.
        let src = include_str!("../../fixtures/sample_ingest.rs");
        let file = syn::parse_file(src).unwrap();
        let mut ran = 0;
        for item in file.items {
            let syn::Item::Fn(func) = item else { continue };
            if !matches!(func.vis, syn::Visibility::Public(_)) {
                continue;
            }
            let name = func.sig.ident.to_string();
            let rec = build_v2_record(&func, None, false, false).unwrap()
                .unwrap_or_else(|| panic!("{name}: a v0.2 record (has doc-tests)"));
            let body = body_ast(&func).unwrap_or_else(|| panic!("{name}: an executable body"));
            // The record's body_hash is the address of exactly this lifted body.
            let addr = nl_validator::hash_artifact_with_kind(&body, nl_validator::ArtifactKind::BodyExpression).unwrap();
            assert_eq!(rec["body_hash"], json!(addr), "{name}: body_hash matches the lifted body");
            let examples = rec["examples"].as_array().unwrap();
            assert!(!examples.is_empty(), "{name}: has mined examples");
            for ex in examples {
                let args: Vec<Value> = ex["args"].as_array().unwrap().clone();
                let got = nl_validator::eval_body(&body, &args)
                    .unwrap_or_else(|e| panic!("{name}: eval failed: {e}"));
                assert_eq!(got, ex["result"], "{name}: on {:?}", ex["args"]);
            }
            // …and it CERTIFIES: the full verified-by-default gate (typecheck / effects /
            // termination / complexity) passes, so the record can enter the commons.
            let cert = nl_validator::certify_record(&rec, &body, &std::collections::HashMap::new(), "z3");
            assert!(cert.certified, "{name}: certifies");
            ran += 1;
        }
        assert_eq!(ran, 9, "all nine sample functions ingest, lift, run, and certify");
    }

    #[test]
    fn body_accumulator_for_loop_lifts_and_executes() {
        let int = |n: i64| json!({ "kind": "int", "value": n });
        let list = |ns: &[i64]| json!({ "kind": "list", "elems": ns.iter().map(|n| int(*n)).collect::<Vec<_>>() });
        // `let mut total = 0; for x in xs { total += x; } total` -> foldl add 0 xs.
        let sum = body_ast(&item_fn("pub fn sum(xs: Vec<i64>) -> i64 { let mut total = 0; for x in xs { total += x; } total }"))
            .expect("accumulator for-loop in subset");
        assert!(sum.to_string().contains("foldl"));
        assert_eq!(nl_validator::eval_body(&sum, &[list(&[1, 2, 3, 4])]).unwrap(), int(10));
        assert_eq!(nl_validator::eval_body(&sum, &[list(&[])]).unwrap(), int(0));
        // `*=` product accumulator.
        let prod = body_ast(&item_fn("pub fn prod(xs: Vec<i64>) -> i64 { let mut acc = 1; for x in xs { acc *= x; } acc }")).unwrap();
        assert_eq!(nl_validator::eval_body(&prod, &[list(&[2, 3, 4])]).unwrap(), int(24));
        // A multi-statement loop body is out of subset (single accumulator statement only).
        let multi = item_fn("pub fn h(xs: Vec<i64>) -> i64 { let mut a = 0; for x in xs { a += x; a += 1; } a }");
        assert!(body_ast(&multi).is_none());
    }

    #[test]
    fn value_some_maps_to_canonical_just() {
        // A doctest `Some(3)` value ingests as the canonical Maybe `Just(3)`, not a `Some` tag —
        // so a Rust-ingested optional matches what every Nova Maybe builtin produces.
        let some: syn::Expr = syn::parse_str("Some(3)").unwrap();
        assert_eq!(
            value_ast(&some, None).unwrap(),
            json!({ "kind": "variant", "tag": "Just", "payload": { "kind": "int", "value": 3 } })
        );
    }
}
