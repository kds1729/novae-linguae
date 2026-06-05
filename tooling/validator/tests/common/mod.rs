//! Shared helpers for the integration test suite.
//!
//! Each file under `tests/` is compiled as its own crate, so this module is
//! `mod common;`-included where needed. `allow(dead_code)` keeps files that
//! use only a subset of the helpers warning-free.
#![allow(dead_code)]

use serde_json::Value;
use std::path::PathBuf;

/// Absolute path to the repo's `spec/` directory, resolved relative to this
/// crate so the tests run from any working directory.
pub fn spec_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec")
}

/// Read and parse a JSON file under `spec/`, panicking with context on failure.
pub fn read(rel: &str) -> Value {
    nl_validator::read_json(&spec_dir().join(rel))
        .unwrap_or_else(|e| panic!("reading spec/{rel}: {e:#}"))
}

/// Read a concrete example artifact from `spec/examples/`.
pub fn example(name: &str) -> Value {
    read(&format!("examples/{name}"))
}

/// Read a schema from `spec/`.
pub fn schema(name: &str) -> Value {
    read(name)
}

// ---- conformance-fixture helpers ----

/// The `spec/conformance/` directory.
pub fn conformance_dir() -> PathBuf {
    spec_dir().join("conformance")
}

/// The parsed conformance manifest.
pub fn manifest() -> Value {
    nl_validator::read_json(&conformance_dir().join("manifest.json"))
        .unwrap_or_else(|e| panic!("reading conformance manifest: {e:#}"))
}

/// Resolve a manifest-relative path (paths in the manifest are relative to the
/// manifest file, which lives in `spec/conformance/`).
pub fn resolve(rel: &str) -> PathBuf {
    conformance_dir().join(rel)
}

/// Read a vector's input, whether supplied as a file path (`input`/`schema`/
/// `record`) or embedded inline (`input_inline`).
pub fn vector_input(v: &Value, file_key: &str) -> Value {
    if let Some(p) = v.get(file_key).and_then(|x| x.as_str()) {
        nl_validator::read_json(&resolve(p))
            .unwrap_or_else(|e| panic!("reading vector input {p}: {e:#}"))
    } else {
        v.get("input_inline")
            .unwrap_or_else(|| panic!("vector has neither `{file_key}` nor `input_inline`"))
            .clone()
    }
}

/// Map a manifest `kind` string to the library enum.
pub fn parse_kind(s: &str) -> nl_validator::ArtifactKind {
    match s {
        "function-record" => nl_validator::ArtifactKind::FunctionRecord,
        "message" => nl_validator::ArtifactKind::Message,
        "body" => nl_validator::ArtifactKind::BodyExpression,
        other => panic!("unknown manifest kind `{other}`"),
    }
}

/// Borrow the `vectors` array under a named manifest section.
pub fn section<'a>(m: &'a Value, name: &str) -> &'a [Value] {
    m[name]["vectors"]
        .as_array()
        .unwrap_or_else(|| panic!("manifest section `{name}` has no `vectors` array"))
}

/// A vector's `name`, for failure messages.
pub fn vname(v: &Value) -> &str {
    v["name"].as_str().unwrap_or("<unnamed>")
}
