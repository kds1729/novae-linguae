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

/// Surface-syntax vectors: parse to the expected AST, validate that AST against
/// the authoritative sub-language schema, and exercise the round-trip contract.
/// Ties `spec/surface-syntax.md` to the committed schemas so parser drift fails
/// CI. v0.1 covers the `type` sub-language.
#[cfg(feature = "surface")]
#[test]
fn surface_vectors_parse_unparse_and_validate() {
    let m = manifest();

    // Map a sub-language to its JSON Schema file under spec/.
    fn schema_file(sub: &str) -> &'static str {
        match sub {
            "type" => "type-expression.schema.json",
            "value" => "value-expression.schema.json",
            "predicate" => "predicate-expression.schema.json",
            "body" => "body-expression.schema.json",
            other => panic!("surface vector: unsupported sub_language `{other}`"),
        }
    }
    // Parse a surface string for the given sub-language.
    fn parse(sub: &str, src: &str) -> serde_json::Value {
        match sub {
            "type" => nl_validator::surface::parse_type(src)
                .unwrap_or_else(|e| panic!("parse_type({src:?}): {e}")),
            "value" => nl_validator::surface::parse_value(src)
                .unwrap_or_else(|e| panic!("parse_value({src:?}): {e}")),
            "predicate" => nl_validator::surface::parse_predicate(src)
                .unwrap_or_else(|e| panic!("parse_predicate({src:?}): {e}")),
            "body" => nl_validator::surface::parse_body(src)
                .unwrap_or_else(|e| panic!("parse_body({src:?}): {e}")),
            other => panic!("surface vector: unsupported sub_language `{other}`"),
        }
    }
    // Pretty-print an AST for the given sub-language.
    fn unparse(sub: &str, ast: &serde_json::Value) -> String {
        match sub {
            "type" => nl_validator::surface::unparse_type(ast)
                .unwrap_or_else(|e| panic!("unparse_type: {e}")),
            "value" => nl_validator::surface::unparse_value(ast)
                .unwrap_or_else(|e| panic!("unparse_value: {e}")),
            "predicate" => nl_validator::surface::unparse_predicate(ast)
                .unwrap_or_else(|e| panic!("unparse_predicate: {e}")),
            "body" => nl_validator::surface::unparse_body(ast)
                .unwrap_or_else(|e| panic!("unparse_body: {e}")),
            other => panic!("surface vector: unsupported sub_language `{other}`"),
        }
    }

    for v in section(&m, "surface_vectors") {
        let name = vname(v);
        let sub = v["sub_language"].as_str().unwrap();
        let surface = v["surface"].as_str().unwrap();
        let expected = &v["expected_ast"];
        let canonical = v["canonical"].as_bool().unwrap_or(false);

        // 1. parse(surface) == expected_ast
        let ast = parse(sub, surface);
        assert_eq!(&ast, expected, "{name}: parsed AST differs from expected_ast");

        // 2. The AST validates against the authoritative schema.
        let schema = common::schema(schema_file(sub));
        validate_with_refs(&schema, &ast, &spec_dir()).unwrap_or_else(|e| {
            panic!(
                "{name}: parsed AST does not validate against {}: {e:#}",
                schema_file(sub)
            )
        });

        // 3. unparse is canonical: when the input is flagged canonical it must
        //    be reproduced byte-for-byte; either way unparse is a fixed point.
        let printed = unparse(sub, &ast);
        if canonical {
            assert_eq!(printed, surface, "{name}: unparse is not the canonical string");
        }
        let reparsed = parse(sub, &printed);
        if canonical {
            // Round-trip identity holds for canonical ASTs.
            assert_eq!(reparsed, ast, "{name}: parse(unparse(ast)) != ast");
        }
        // Canonical form is idempotent for every vector, canonical or not.
        assert_eq!(
            unparse(sub, &reparsed),
            printed,
            "{name}: unparse is not idempotent"
        );
    }
}
