//! `nl_validator`: library for validating Novae Linguae artifacts against
//! their JSON Schemas.
//!
//! This is the reference implementation. Other implementations of the
//! validator MUST produce identical pass/fail decisions for any valid
//! schema/instance pair; the conformance vectors at `spec/conformance/` are
//! the contract that pins this across implementations.
//!
//! Subsequent versions of this crate may add:
//! - Well-formedness checks for the refinement / property / value sub-languages
//!   (their schemas already exist; the checks await the verifier engine, as
//!   `check_type_well_formed` already does for type expressions).

use anyhow::{anyhow, Context, Result};
use jsonschema::{Retrieve, Uri};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Read and parse a UTF-8 JSON file from disk.
pub fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing JSON from {}", path.display()))
}

/// Validate a JSON instance against a JSON Schema 2020-12 schema.
///
/// Resolves only same-document (`#/...`) references. For schemas that reference
/// sibling schema files (cross-file `$ref`), use [`validate_with_refs`].
///
/// Returns `Ok(())` on success. On failure, returns an error whose display
/// form contains every validation error, one per line, with instance-path
/// pointers.
pub fn validate(schema: &Value, instance: &Value) -> Result<()> {
    let validator = jsonschema::draft202012::new(schema)
        .map_err(|e| anyhow!("compiling schema: {e}"))?;
    collect_errors(&validator, instance)
}

/// Validate against a schema that may contain cross-file `$ref`s into the
/// Novae Linguae schema namespace (`https://novae-linguae.org/spec/...`).
///
/// References are resolved by [`LocalSchemaRetriever`] against `spec_dir`: the
/// logical schema identifier's filename component is looked up as a sibling
/// file there. Schemas without external references validate identically to
/// [`validate`] — the retriever is simply never consulted.
pub fn validate_with_refs(schema: &Value, instance: &Value, spec_dir: &Path) -> Result<()> {
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_retriever(LocalSchemaRetriever {
            spec_dir: spec_dir.to_path_buf(),
        })
        .build(schema)
        .map_err(|e| anyhow!("compiling schema: {e}"))?;
    collect_errors(&validator, instance)
}

/// Run a built validator over an instance and fold any errors into a single
/// human-readable `anyhow` error.
fn collect_errors(validator: &jsonschema::Validator, instance: &Value) -> Result<()> {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| format!("  - at {}: {}", e.instance_path, e))
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        let count = errors.len();
        Err(anyhow!(
            "validation failed ({} error{}):\n{}",
            count,
            if count == 1 { "" } else { "s" },
            errors.join("\n")
        ))
    }
}

/// Logical base of every Novae Linguae schema `$id` — e.g.
/// `https://novae-linguae.org/spec/v0.1/type-expression.schema.json`. Schemas
/// identify themselves by this stable namespace rather than by filesystem path,
/// so the commons stays location-independent; cross-file `$ref`s resolve
/// against the referring schema's `$id` into this same namespace.
const SCHEMA_ID_BASE: &str = "https://novae-linguae.org/spec/";

/// Resolves cross-file schema `$ref`s from a local `spec/` directory.
///
/// A reference resolves to a URI like
/// `https://novae-linguae.org/spec/v0.1/function-record.schema.json`; this
/// retriever maps it to `<spec_dir>/function-record.schema.json`. The version
/// path segment (`v0.1`, `v0.2`) is logical only — all schema files live flat
/// in `spec/`, and the schema's own `$id` carries the version it speaks for.
struct LocalSchemaRetriever {
    spec_dir: PathBuf,
}

impl Retrieve for LocalSchemaRetriever {
    fn retrieve(
        &self,
        uri: &Uri<&str>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let s = uri.as_str();
        let rest = s.strip_prefix(SCHEMA_ID_BASE).ok_or_else(|| {
            format!(
                "cannot resolve external $ref `{s}`: only Novae Linguae schema URIs under `{SCHEMA_ID_BASE}` resolve locally"
            )
        })?;
        let file = rest.rsplit('/').next().filter(|f| !f.is_empty()).ok_or_else(|| {
            format!("cannot resolve external $ref `{s}`: no schema filename in the URI")
        })?;
        let path = self.spec_dir.join(file);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("resolving $ref `{s}` -> {}: {e}", path.display()))?;
        let value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing referenced schema {}: {e}", path.display()))?;
        Ok(value)
    }
}

/// JCS-canonicalize a JSON value to UTF-8 bytes per RFC 8785.
///
/// This is the canonical-form bytes referred to throughout
/// `spec/canonical-serialization.md`. The output:
/// - sorts all object keys lexicographically by UTF-16 code unit;
/// - contains no whitespace between tokens;
/// - is UTF-8 with no byte-order mark and no trailing newline;
/// - uses ECMAScript number serialization rules per JCS §3.2.2.3.
///
/// This function does NOT remove any fields. Field-removal-before-hashing
/// (e.g. stripping `hash` and `signature` for messages) is the caller's
/// responsibility, performed before invoking `canonicalize`.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>> {
    serde_jcs::to_vec(value).map_err(|e| anyhow!("JCS canonicalization failed: {e}"))
}

// ---- artifact kind detection and field stripping ----

/// Identifies what kind of Novae Linguae artifact a JSON value represents.
/// Determines which fields to strip before hashing and which prefix to use
/// when rendering the resulting hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    FunctionRecord,
    Message,
    BodyExpression,
}

impl ArtifactKind {
    /// Fields stripped from the artifact before JCS-canonicalizing and hashing,
    /// per `spec/canonical-serialization.md`. Body expressions have no embedded
    /// `hash` field — the whole expression IS what gets hashed.
    fn strip_fields(self) -> &'static [&'static str] {
        match self {
            ArtifactKind::FunctionRecord => &["hash"],
            ArtifactKind::Message => &["hash", "signature"],
            ArtifactKind::BodyExpression => &[],
        }
    }

    /// Content-address prefix used when rendering the hash.
    pub fn prefix(self) -> &'static str {
        match self {
            ArtifactKind::FunctionRecord => "fn",
            ArtifactKind::Message => "msg",
            ArtifactKind::BodyExpression => "expr",
        }
    }

    /// Auto-detect the artifact kind from the JSON shape.
    ///
    /// - A *Nova Locutio* message has a top-level `kind` field whose value is
    ///   one of the v0.1 speech acts.
    /// - A body expression has a top-level `kind` field whose value is one of
    ///   the v0.1 body-expression kinds (`var`, `lit`, `app`, `let`, `lambda`,
    ///   `case`, `field`).
    /// - A function record does not have a `kind` field but has both
    ///   `signature` and `body_hash`.
    ///
    /// Type expressions and predicate expressions overlap with body-expression
    /// on some kind names (e.g. `var`, `app`) but are not independently
    /// hashable in v0.1 — they live as embedded sub-trees of function records.
    /// At this layer, those kinds are assumed to be body expressions; pass an
    /// explicit kind to `hash_artifact_with_kind` if you need otherwise.
    pub fn detect(value: &Value) -> Result<Self> {
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow!("expected JSON object at top level"))?;

        if let Some(kind_str) = obj.get("kind").and_then(|v| v.as_str()) {
            const SPEECH_ACTS: &[&str] = &[
                "request", "assert", "query", "propose", "commit", "retract", "delegate", "ack",
                "reject",
            ];
            if SPEECH_ACTS.contains(&kind_str) {
                return Ok(ArtifactKind::Message);
            }
            const BODY_KINDS: &[&str] = &[
                "var", "lit", "app", "let", "lambda", "case", "field",
            ];
            if BODY_KINDS.contains(&kind_str) {
                return Ok(ArtifactKind::BodyExpression);
            }
            return Err(anyhow!(
                "cannot auto-detect artifact kind from top-level `kind` = `{kind_str}`. Not a Nova Locutio speech act and not a body-expression kind. Type expressions and predicate expressions are not independently hashable at this layer in v0.1."
            ));
        }

        if obj.contains_key("signature") && obj.contains_key("body_hash") {
            return Ok(ArtifactKind::FunctionRecord);
        }

        Err(anyhow!(
            "could not detect artifact kind from JSON shape — expected a function record (has 'signature' and 'body_hash'), a Nova Locutio message (top-level `kind` is a speech act), or a body expression (top-level `kind` is one of var/lit/app/let/lambda/case/field)"
        ))
    }
}

/// Return a copy of `value` with the fields stripped that would be removed
/// before hashing for the given artifact kind.
pub fn strip_for_hash(value: &Value, kind: ArtifactKind) -> Value {
    match value {
        Value::Object(map) => {
            let mut cloned = map.clone();
            for field in kind.strip_fields() {
                cloned.remove(*field);
            }
            Value::Object(cloned)
        }
        _ => value.clone(),
    }
}

// ---- BLAKE3-256 hashing ----

/// BLAKE3-256 hash of arbitrary bytes. Returns the 32 raw bytes of the digest.
pub fn blake3_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Render a 32-byte hash as `<prefix>_<64 lowercase hex chars>`.
pub fn format_hash(prefix: &str, hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(prefix.len() + 1 + 64);
    out.push_str(prefix);
    out.push('_');
    for byte in hash {
        write!(out, "{:02x}", byte).expect("writing to String is infallible");
    }
    out
}

/// Compute the content-hash of an artifact end-to-end, with the given kind:
/// strip the kind-appropriate fields, JCS-canonicalize, BLAKE3-256, and format
/// with the kind's prefix. Returns e.g. `fn_<hex>`, `msg_<hex>`, `expr_<hex>`.
pub fn hash_artifact_with_kind(value: &Value, kind: ArtifactKind) -> Result<String> {
    let stripped = strip_for_hash(value, kind);
    let canonical = canonicalize(&stripped)?;
    let hash = blake3_hash(&canonical);
    Ok(format_hash(kind.prefix(), &hash))
}

/// Compute the content-hash of an artifact, auto-detecting its kind from the
/// JSON shape. See `ArtifactKind::detect` for the detection rules.
pub fn hash_artifact(value: &Value) -> Result<String> {
    let kind = ArtifactKind::detect(value)?;
    hash_artifact_with_kind(value, kind)
}

// ---- hash verification ----

/// Result of comparing an artifact's stored `hash` field to its recomputed
/// content-hash.
#[derive(Debug, Clone)]
pub struct HashVerification {
    /// The hash recorded in the artifact's `hash` field, if any. `None` means
    /// the artifact had no `hash` field at all.
    pub stored: Option<String>,
    /// The hash computed from the artifact's current contents.
    pub computed: String,
    /// True iff a stored hash existed and equals the computed hash.
    pub matches: bool,
}

/// Verify an artifact's stored `hash` against its recomputed content-hash,
/// using the supplied kind.
pub fn verify_artifact_hash_with_kind(
    value: &Value,
    kind: ArtifactKind,
) -> Result<HashVerification> {
    let stored = value
        .get("hash")
        .and_then(|v| v.as_str())
        .map(String::from);
    let computed = hash_artifact_with_kind(value, kind)?;
    let matches = stored.as_deref() == Some(computed.as_str());
    Ok(HashVerification {
        stored,
        computed,
        matches,
    })
}

/// Verify an artifact's stored `hash` against its recomputed content-hash,
/// auto-detecting the kind from the JSON shape.
pub fn verify_artifact_hash(value: &Value) -> Result<HashVerification> {
    let kind = ArtifactKind::detect(value)?;
    verify_artifact_hash_with_kind(value, kind)
}

// ---- Ed25519 signing and verification (Nova Locutio messages) ----

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Derive a deterministic Ed25519 signing key from a seed string. The seed is
/// BLAKE3-hashed to 32 bytes which become the secret-key scalar. Identical
/// seeds always produce identical keypairs — useful for reproducible
/// examples, harmless as a security matter when the seed itself is public.
pub fn signing_key_from_seed(seed: &str) -> SigningKey {
    let h = blake3_hash(seed.as_bytes());
    SigningKey::from_bytes(&h)
}

/// Format an Ed25519 verifying key as `did:nova:<64-hex>`, the v0.1 DID method
/// for Novae Linguae. The 64 hex chars are the raw 32-byte Ed25519 public key,
/// which lets a receiver extract the public key from the DID without any
/// resolver lookup.
pub fn did_nova_from_pubkey(pubkey: &VerifyingKey) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity("did:nova:".len() + 64);
    s.push_str("did:nova:");
    for byte in pubkey.as_bytes() {
        write!(s, "{:02x}", byte).expect("writing to String is infallible");
    }
    s
}

/// Parse a `did:nova:<64-hex>` DID and extract its embedded Ed25519 verifying
/// key. Other DID methods (e.g. `did:key:`) are not supported in v0.1.
pub fn pubkey_from_did_nova(did: &str) -> Result<VerifyingKey> {
    let suffix = did
        .strip_prefix("did:nova:")
        .ok_or_else(|| anyhow!("v0.1 only supports did:nova: DIDs; got {did}"))?;
    if suffix.len() != 64 {
        return Err(anyhow!(
            "did:nova suffix must be 64 hex chars, got {} chars in {did}",
            suffix.len()
        ));
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&suffix[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("invalid hex in DID {did}: {e}"))?;
    }
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| anyhow!("DID does not encode a valid Ed25519 public key: {e}"))
}

/// Encode an Ed25519 signature as `ed25519:<base64>`.
pub fn format_signature(sig: &Signature) -> String {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    format!("ed25519:{}", engine.encode(sig.to_bytes()))
}

/// Parse an `ed25519:<base64>` signature string into an Ed25519 signature.
pub fn parse_signature(s: &str) -> Result<Signature> {
    use base64::Engine;
    let b64 = s
        .strip_prefix("ed25519:")
        .ok_or_else(|| anyhow!("signature must start with 'ed25519:': {s}"))?;
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine
        .decode(b64)
        .map_err(|e| anyhow!("invalid base64 in signature: {e}"))?;
    if bytes.len() != 64 {
        return Err(anyhow!(
            "Ed25519 signature must be 64 bytes; got {}",
            bytes.len()
        ));
    }
    let arr: [u8; 64] = bytes
        .try_into()
        .map_err(|_| anyhow!("signature byte conversion failed"))?;
    Ok(Signature::from_bytes(&arr))
}

/// Sign a Nova Locutio message in place. Sets:
/// 1. `from` to the `did:nova:<hex>` of the signing key's public key.
/// 2. `hash` to BLAKE3-256(canonical(msg − {hash, signature})), prefixed `msg_`.
/// 3. `signature` to ed25519:<base64-of-Ed25519(canonical(msg − {signature}))>.
///
/// The hash is included in what is signed, so signature also covers the hash.
/// Both transformations operate on the same JSON object; the caller passes a
/// mutable reference.
pub fn sign_message(value: &mut Value, signing_key: &SigningKey) -> Result<()> {
    let pubkey = signing_key.verifying_key();
    let did = did_nova_from_pubkey(&pubkey);

    // Set `from` to match the signing identity.
    let obj = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("expected JSON object at top level"))?;
    obj.insert("from".to_string(), Value::String(did));

    // Compute and set `hash` = BLAKE3(canonical(msg − {hash, signature})).
    let mut for_hash = Value::Object(obj.clone());
    if let Some(map) = for_hash.as_object_mut() {
        map.remove("hash");
        map.remove("signature");
    }
    let canonical_h = canonicalize(&for_hash)?;
    let h = blake3_hash(&canonical_h);
    let hash_str = format_hash("msg", &h);
    obj.insert("hash".to_string(), Value::String(hash_str));

    // Compute and set `signature` = Ed25519(canonical(msg − {signature})).
    // The hash field IS included in the signed bytes.
    let mut for_sig = Value::Object(obj.clone());
    if let Some(map) = for_sig.as_object_mut() {
        map.remove("signature");
    }
    let canonical_s = canonicalize(&for_sig)?;
    let sig = signing_key.sign(&canonical_s);
    obj.insert("signature".to_string(), Value::String(format_signature(&sig)));

    Ok(())
}

/// Verify the Ed25519 signature on a message. Extracts the public key from the
/// `from` DID, recomputes canonical(msg − {signature}), and checks the
/// signature against the public key.
pub fn verify_signature(value: &Value) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("expected JSON object at top level"))?;

    let from = obj
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("message has no `from` field"))?;
    let pubkey = pubkey_from_did_nova(from)
        .context("resolving public key from `from` DID")?;

    let sig_str = obj
        .get("signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("message has no `signature` field"))?;
    let signature = parse_signature(sig_str).context("parsing `signature` field")?;

    let mut for_sig = Value::Object(obj.clone());
    if let Some(map) = for_sig.as_object_mut() {
        map.remove("signature");
    }
    let signed_bytes = canonicalize(&for_sig)?;

    pubkey
        .verify(&signed_bytes, &signature)
        .map_err(|e| anyhow!("Ed25519 signature verification failed: {e}"))
}

// ---- well-formedness checks for type expressions ----

/// Check well-formedness of a Nova Lingua type expression. Validates rules
/// that JSON Schema cannot express on its own:
///
/// - **Type-variable scoping**: every `var` is bound by an enclosing `forall`.
/// - **Rank-1 polymorphism**: `forall` appears only at the outermost position
///   of a type, never nested inside function-argument positions or other
///   inner positions.
/// - **Uniqueness within `record.fields`**: field names are unique.
/// - **Uniqueness within `sum.variants`**: variant tags are unique.
/// - **`apply.ctor` kind compatibility**: the ctor must itself be a `var`,
///   `ref`, `builtin`, or `apply` (chained partial application). Concrete
///   types like `fn`, `tuple`, `record`, `sum` are not type constructors and
///   cannot appear in `ctor` position.
///
/// Does NOT re-check anything JSON Schema already enforces. Run `validate`
/// against `type-expression.schema.json` first; this is the second pass.
pub fn check_type_well_formed(value: &Value) -> Result<()> {
    check_type_node(value, &[], true)
}

fn check_type_node(value: &Value, bound_vars: &[String], allow_forall: bool) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("type expression must be a JSON object"))?;
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("type expression is missing the `kind` field"))?;

    match kind {
        "var" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("`var` is missing `name`"))?;
            if !bound_vars.iter().any(|v| v == name) {
                return Err(anyhow!(
                    "type variable `{name}` is not bound by any enclosing forall (in scope: [{}])",
                    bound_vars.join(", ")
                ));
            }
        }
        "ref" | "builtin" => {
            // Leaves; no recursion needed. JSON Schema has already validated
            // the shape and any enum constraints.
        }
        "forall" => {
            if !allow_forall {
                return Err(anyhow!(
                    "forall is only allowed at the outermost position of a type (rank-1 polymorphism, v0.1)"
                ));
            }
            let vars = obj
                .get("vars")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`forall` is missing `vars`"))?;
            let mut new_bound: Vec<String> = bound_vars.to_vec();
            for v in vars {
                let name = v
                    .as_str()
                    .ok_or_else(|| anyhow!("`forall.vars[]` entries must be strings"))?;
                new_bound.push(name.to_string());
            }
            let body = obj
                .get("body")
                .ok_or_else(|| anyhow!("`forall` is missing `body`"))?;
            // Forall body cannot contain another forall in v0.1.
            check_type_node(body, &new_bound, false)?;
        }
        "fn" => {
            let params = obj
                .get("params")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`fn` is missing `params`"))?;
            for p in params {
                check_type_node(p, bound_vars, false)?;
            }
            let result = obj
                .get("result")
                .ok_or_else(|| anyhow!("`fn` is missing `result`"))?;
            check_type_node(result, bound_vars, false)?;
        }
        "apply" => {
            let ctor = obj
                .get("ctor")
                .ok_or_else(|| anyhow!("`apply` is missing `ctor`"))?;
            let ctor_kind = ctor
                .as_object()
                .and_then(|o| o.get("kind"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("`apply.ctor` must be a type expression with a `kind`"))?;
            match ctor_kind {
                "var" | "ref" | "builtin" | "apply" => {}
                other => {
                    return Err(anyhow!(
                        "`apply.ctor` must be of kind var | ref | builtin | apply; got `{other}` (kind `{other}` is not a type constructor)"
                    ));
                }
            }
            check_type_node(ctor, bound_vars, false)?;
            let args = obj
                .get("args")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`apply` is missing `args`"))?;
            for a in args {
                check_type_node(a, bound_vars, false)?;
            }
        }
        "tuple" => {
            let elems = obj
                .get("elems")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`tuple` is missing `elems`"))?;
            for e in elems {
                check_type_node(e, bound_vars, false)?;
            }
        }
        "record" => {
            let fields = obj
                .get("fields")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`record` is missing `fields`"))?;
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for f in fields {
                let name = f
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("`record.fields[]` entries must have `name`"))?;
                if !seen.insert(name) {
                    return Err(anyhow!("record field `{name}` appears more than once"));
                }
                let ty = f
                    .get("type")
                    .ok_or_else(|| anyhow!("`record.fields[].type` is required"))?;
                check_type_node(ty, bound_vars, false)?;
            }
        }
        "sum" => {
            let variants = obj
                .get("variants")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("`sum` is missing `variants`"))?;
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for v in variants {
                let tag = v
                    .get("tag")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| anyhow!("`sum.variants[]` entries must have `tag`"))?;
                if !seen.insert(tag) {
                    return Err(anyhow!("sum variant tag `{tag}` appears more than once"));
                }
                if let Some(t) = v.get("type") {
                    check_type_node(t, bound_vars, false)?;
                }
            }
        }
        other => {
            return Err(anyhow!(
                "unknown type-expression kind `{other}` (expected one of: var, ref, builtin, forall, fn, apply, tuple, record, sum)"
            ));
        }
    }
    Ok(())
}
