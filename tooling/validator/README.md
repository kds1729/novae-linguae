# nl-validator

Reference command-line validator, canonicalizer, and hasher for *Novae Linguae* artifacts.

## Status

**v0.1, in progress.** Implemented checkboxes are live; unchecked are scoped for follow-up commits.

- [x] Validate a JSON instance against a JSON Schema (draft 2020-12).
- [ ] JCS-canonicalize a record per [`spec/canonical-serialization.md`](../../spec/canonical-serialization.md).
- [ ] BLAKE3-256 hash a canonicalized record.
- [ ] Verify the `hash` field on a record matches its computed hash.
- [ ] Verify Ed25519 signatures on *Nova Locutio* messages.
- [ ] Well-formedness checks beyond JSON Schema (type-variable scoping, uniqueness within sums and records, ctor-kind compatibility in `apply`).
- [ ] Conformance test suite (record → canonical bytes → hash) suitable for cross-implementation byte-equality testing.

## Build

Requires a recent Rust toolchain (1.75+ recommended).

```bash
cd tooling/validator
cargo build --release
```

The compiled binary is at `target/release/nl-validator`.

## Usage

```bash
# Validate the example map function record against the function-record schema.
target/release/nl-validator validate \
    ../../spec/function-record.schema.json \
    ../../spec/examples/map.json

# Validate an example message.
target/release/nl-validator validate \
    ../../spec/message.schema.json \
    ../../spec/examples/request.json

# Validate the example type expression.
target/release/nl-validator validate \
    ../../spec/type-expression.schema.json \
    ../../spec/examples/type-map.json
```

Exit code 0 on success, non-zero on failure. Validation diagnostics go to stderr; one line per error with a JSON Pointer to the failing instance location.

## Why Rust

For a reference implementation:

- Fast; produces a single static binary via `cargo build --release`.
- Mature, well-maintained crates for everything we need: `jsonschema`, `serde_jcs`, `blake3`, `ed25519-dalek`.
- Aligned with the eventual ingestion-from-Rust path (the first ingestion adapter target is Rust crates).

Other implementations are welcome (Deno/TypeScript, Python, Go). All implementations MUST agree byte-for-byte on canonical form and hash. The forthcoming conformance test suite will pin this contract.

## Crate version notes

The `jsonschema` crate has gone through API changes over recent versions. This crate currently pins `jsonschema = "0.28"` and uses the `jsonschema::draft202012::new` constructor. If you upgrade the dependency, expect to adjust call sites in `src/lib.rs`.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
