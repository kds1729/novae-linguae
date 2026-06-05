//! Manifest-driven conformance suite. Every vector in
//! `spec/conformance/manifest.json` is replayed against the reference
//! implementation. This is the same contract any other implementation must
//! satisfy — the reference impl is just the first consumer of its own fixtures.
//!
//! These tests subsume the old hardcoded golden-hash constants: the expected
//! values now live in the manifest, not in Rust source.

mod common;

use common::{manifest, parse_kind, resolve, section, spec_dir, vector_input, vname};
use nl_validator::{
    canonicalize, check_type_well_formed, hash_artifact_with_kind, signing_key_from_seed,
    sign_message, strip_for_hash, validate_with_refs, verify_signature,
};

#[test]
fn hash_vectors_reproduce_canonical_bytes_and_hashes() {
    let m = manifest();
    let vectors = section(&m, "hash_vectors");
    assert!(!vectors.is_empty(), "no hash vectors in manifest");

    for v in vectors {
        let name = vname(v);
        let input = vector_input(v, "input");
        let kind = parse_kind(v["kind"].as_str().unwrap());

        // Canonical preimage must match the committed .jcs bytes exactly.
        let preimage_path = resolve(v["canonical_preimage"].as_str().unwrap());
        let expected_bytes = std::fs::read(&preimage_path)
            .unwrap_or_else(|e| panic!("{name}: reading {}: {e}", preimage_path.display()));
        let computed_bytes = canonicalize(&strip_for_hash(&input, kind)).unwrap();
        assert_eq!(
            computed_bytes, expected_bytes,
            "{name}: canonical preimage bytes differ from {}",
            preimage_path.display()
        );

        // And the hash must match.
        let computed_hash = hash_artifact_with_kind(&input, kind).unwrap();
        assert_eq!(
            computed_hash,
            v["expected_hash"].as_str().unwrap(),
            "{name}: hash mismatch"
        );
    }
}

#[test]
fn cross_reference_vectors_hold() {
    let m = manifest();

    // Build name -> expected_hash from the hash vectors.
    let lookup = |target: &str| -> String {
        section(&m, "hash_vectors")
            .iter()
            .find(|hv| vname(hv) == target)
            .unwrap_or_else(|| panic!("hash vector `{target}` not found"))["expected_hash"]
            .as_str()
            .unwrap()
            .to_string()
    };

    for v in section(&m, "cross_reference_vectors") {
        let name = vname(v);
        let record = vector_input(v, "record");
        let field = v["field"].as_str().unwrap();
        let actual = record[field]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: record has no string field `{field}`"));
        let expected = lookup(v["must_equal_hash_vector"].as_str().unwrap());
        assert_eq!(actual, expected, "{name}: {field} does not match referenced hash");
    }
}

#[test]
fn signing_vectors_are_deterministic() {
    let m = manifest();
    for v in section(&m, "signing_vectors") {
        let name = vname(v);
        let mut msg = vector_input(v, "input");
        let key = signing_key_from_seed(v["seed"].as_str().unwrap());
        sign_message(&mut msg, &key).unwrap();

        assert_eq!(msg["from"], v["expected_from"], "{name}: from mismatch");
        assert_eq!(msg["hash"], v["expected_hash"], "{name}: hash mismatch");
        assert_eq!(
            msg["signature"], v["expected_signature"],
            "{name}: signature mismatch"
        );
    }
}

#[test]
fn signature_verification_vectors_hold() {
    let m = manifest();
    for v in section(&m, "signature_verification_vectors") {
        let name = vname(v);
        let msg = vector_input(v, "input");
        let ok = verify_signature(&msg).is_ok();
        match v["expected"].as_str().unwrap() {
            "valid" => assert!(ok, "{name}: expected signature to verify"),
            "invalid" => assert!(!ok, "{name}: expected signature NOT to verify"),
            other => panic!("{name}: unknown expected `{other}`"),
        }
    }
}

#[test]
fn type_wellformedness_vectors_hold() {
    let m = manifest();
    for v in section(&m, "type_wellformedness_vectors") {
        let name = vname(v);
        let ty = vector_input(v, "input");
        let ok = check_type_well_formed(&ty).is_ok();
        match v["expected"].as_str().unwrap() {
            "well-formed" => assert!(ok, "{name}: expected well-formed"),
            "ill-formed" => assert!(!ok, "{name}: expected ill-formed"),
            other => panic!("{name}: unknown expected `{other}`"),
        }
    }
}

#[test]
fn schema_validation_vectors_hold() {
    let m = manifest();
    for v in section(&m, "schema_validation_vectors") {
        let name = vname(v);
        let schema = vector_input(v, "schema");
        let instance = vector_input(v, "input");
        // Cross-file `$ref`s (e.g. message payload validation) resolve against spec/.
        let ok = validate_with_refs(&schema, &instance, &spec_dir()).is_ok();
        match v["expected"].as_str().unwrap() {
            "valid" => assert!(ok, "{name}: expected instance to validate"),
            "invalid" => assert!(!ok, "{name}: expected instance to fail validation"),
            other => panic!("{name}: unknown expected `{other}`"),
        }
    }
}
