//! `nl_validator`: library for validating Novae Linguae artifacts against
//! their JSON Schemas.
//!
//! This is the reference implementation. Other implementations of the
//! validator MUST produce identical pass/fail decisions for any valid
//! schema/instance pair. A cross-implementation conformance test suite
//! will pin this once a second implementation exists.
//!
//! Subsequent versions of this crate will add:
//! - BLAKE3-256 hashing
//! - End-to-end record hash verification
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
