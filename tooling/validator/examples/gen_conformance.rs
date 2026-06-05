//! Regenerates the canonical-form preimage fixtures under
//! `spec/conformance/canonical/`. Run from anywhere with:
//!
//! ```bash
//! cargo run --example gen_conformance
//! ```
//!
//! Only the `.jcs` preimage files are generated here. The manifest
//! (`spec/conformance/manifest.json`) and the conformance README are
//! hand-maintained — they are the contract, these files are its evidence.
//!
//! A `.jcs` file is the exact byte sequence that gets BLAKE3-256-hashed: the
//! input artifact with its kind-appropriate fields stripped, then JCS-
//! canonicalized (no trailing newline). An independent implementation that
//! reproduces these bytes — and the hashes in the manifest — is conformant.

use nl_validator::{
    canonicalize, hash_artifact_with_kind, read_json, strip_for_hash, ArtifactKind,
};
use std::path::PathBuf;

fn main() {
    let spec = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec");
    let out = spec.join("conformance/canonical");
    std::fs::create_dir_all(&out).expect("creating conformance/canonical");

    // (example file, output .jcs file, artifact kind)
    let artifacts = [
        ("map.json", "map.jcs", ArtifactKind::FunctionRecord),
        ("double.v0.2.json", "double.v0.2.jcs", ArtifactKind::FunctionRecord),
        ("request.json", "request.jcs", ArtifactKind::Message),
        ("assert.json", "assert.jcs", ArtifactKind::Message),
        ("store-request.json", "store-request.jcs", ArtifactKind::Message),
        ("body-double.json", "body-double.jcs", ArtifactKind::BodyExpression),
    ];

    for (src, dst, kind) in artifacts {
        let v = read_json(&spec.join("examples").join(src))
            .unwrap_or_else(|e| panic!("reading {src}: {e:#}"));
        let bytes = canonicalize(&strip_for_hash(&v, kind)).expect("canonicalize");
        std::fs::write(out.join(dst), &bytes).expect("writing .jcs");
        let hash = hash_artifact_with_kind(&v, kind).expect("hash");
        println!("{dst:24} {:>4} bytes  {hash}", bytes.len());
    }
}
