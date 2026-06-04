//! `nl_validator`: library for validating Novae Linguae artifacts against
//! their JSON Schemas.
//!
//! This is the reference implementation. Other implementations of the
//! validator MUST produce identical pass/fail decisions for any valid
//! schema/instance pair. A cross-implementation conformance test suite
//! will pin this once a second implementation exists.
//!
//! Subsequent versions of this crate will add:
//! - End-to-end record hash verification (`hash_artifact` is in place; the
//!   compare-against-stored-`hash`-field step is the next commit's job)
//! - Ed25519 signature verification for Nova Locutio messages
//! - Well-formedness checks beyond JSON Schema (type-var scoping, uniqueness,
//!   ctor-kind compatibility)

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Read and parse a UTF-8 JSON file from disk.
pub fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing JSON from {}", path.display()))
}

/// Validate a JSON instance against a JSON Schema 2020-12 schema.
///
/// Returns `Ok(())` on success. On failure, returns an error whose display
/// form contains every validation error, one per line, with instance-path
/// pointers.
pub fn validate(schema: &Value, instance: &Value) -> Result<()> {
    let validator = jsonschema::draft202012::new(schema)
        .map_err(|e| anyhow!("compiling schema: {e}"))?;

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
}

impl ArtifactKind {
    /// Fields stripped from the artifact before JCS-canonicalizing and hashing,
    /// per `spec/canonical-serialization.md`.
    fn strip_fields(self) -> &'static [&'static str] {
        match self {
            ArtifactKind::FunctionRecord => &["hash"],
            ArtifactKind::Message => &["hash", "signature"],
        }
    }

    /// Content-address prefix used when rendering the hash.
    pub fn prefix(self) -> &'static str {
        match self {
            ArtifactKind::FunctionRecord => "fn",
            ArtifactKind::Message => "msg",
        }
    }

    /// Auto-detect the artifact kind from the JSON shape.
    ///
    /// A *Nova Locutio* message has a top-level `kind` field whose value is
    /// one of the v0.1 speech acts. A function record does not have a `kind`
    /// field but has both `signature` and `body_hash`. v0.1 supports these
    /// two independently-hashable artifact kinds; type expressions are
    /// embedded sub-values and are not hashed at the top level.
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
        }

        if obj.contains_key("signature") && obj.contains_key("body_hash") {
            return Ok(ArtifactKind::FunctionRecord);
        }

        Err(anyhow!(
            "could not detect artifact kind from JSON shape — expected a function record (has 'signature' and 'body_hash') or a Nova Locutio message (has 'kind' with a v0.1 speech-act value)"
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

/// Compute the content-hash of an artifact end-to-end:
/// detect kind, strip the appropriate fields, JCS-canonicalize, BLAKE3-256,
/// and format with the kind's prefix. Returns e.g. `fn_<hex>` or `msg_<hex>`.
pub fn hash_artifact(value: &Value) -> Result<String> {
    let kind = ArtifactKind::detect(value)?;
    let stripped = strip_for_hash(value, kind);
    let canonical = canonicalize(&stripped)?;
    let hash = blake3_hash(&canonical);
    Ok(format_hash(kind.prefix(), &hash))
}
