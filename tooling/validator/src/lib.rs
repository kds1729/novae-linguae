//! `nl_validator`: library for validating Novae Linguae artifacts against
//! their JSON Schemas.
//!
//! This is the reference implementation. Other implementations of the
//! validator MUST produce identical pass/fail decisions for any valid
//! schema/instance pair. A cross-implementation conformance test suite
//! will pin this once a second implementation exists.
//!
//! Subsequent versions of this crate will add:
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

/// Verify an artifact's stored `hash` against its recomputed content-hash.
///
/// Returns `Ok(HashVerification { … })` with all three fields populated. The
/// caller decides how to interpret a `None` stored hash or a `matches: false`
/// result. This function returns `Err` only when the artifact cannot be
/// hashed at all (e.g. shape isn't a recognized kind).
pub fn verify_artifact_hash(value: &Value) -> Result<HashVerification> {
    let stored = value
        .get("hash")
        .and_then(|v| v.as_str())
        .map(String::from);
    let computed = hash_artifact(value)?;
    let matches = stored.as_deref() == Some(computed.as_str());
    Ok(HashVerification {
        stored,
        computed,
        matches,
    })
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
