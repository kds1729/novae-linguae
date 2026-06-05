//! nl-ingest: Parse public Rust functions and emit Nova Lingua v0.1 function records.
//!
//! Each public top-level `pub fn` in the given source files becomes one JSON record on
//! stdout (JSONL by default, `--pretty` for readable). The record satisfies the
//! function-record.schema.json structural requirements; hash and body_hash are real
//! BLAKE3 digests, though body_hash is the hash of the function's token stream rather
//! than a proper Nova Lingua body-expression AST — that translation is future work.
//!
//! CAVEATS (all addressable in future iterations):
//!   - Only top-level `pub fn` items are ingested; methods in `impl` blocks are skipped.
//!   - `examples.args` contains one null per parameter; `result` is null. Fill in real values.
//!   - `signature.terminates` is always "unknown". Static analysis is future work.
//!   - `effects`, `properties`, `intent_tags` are empty; add them after ingestion.
//!   - Generic lifetime params are stripped from the type string.

use anyhow::{Context, Result};
use clap::Parser;
use nl_validator::{blake3_hash, canonicalize, format_hash};
use quote::ToTokens;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
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
        for item in ast.items {
            if let Item::Fn(func) = item {
                if matches!(func.vis, syn::Visibility::Public(_)) {
                    let record = build_record(&func, cli.crate_name.as_deref())
                        .with_context(|| {
                            format!(
                                "building record for `{}` in {}",
                                func.sig.ident,
                                path.display()
                            )
                        })?;
                    if cli.pretty {
                        println!("{}", serde_json::to_string_pretty(&record)?);
                    } else {
                        println!("{}", serde_json::to_string(&record)?);
                    }
                }
            }
        }
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
