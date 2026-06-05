# nl-validator

Reference command-line validator, canonicalizer, and hasher for *Novae Linguae* artifacts.

## Status

**v0.1, in progress.** Implemented checkboxes are live; unchecked are scoped for follow-up commits.

- [x] Validate a JSON instance against a JSON Schema (draft 2020-12).
- [x] JCS-canonicalize a record per [`spec/canonical-serialization.md`](../../spec/canonical-serialization.md).
- [x] BLAKE3-256 hash a canonicalized record (`hash` subcommand; auto-detects function-record vs message vs body-expression; pass `--kind <function-record|message|body>` to override).
- [x] Verify the `hash` field on a record matches its computed hash (`verify` subcommand; refused for body-expressions since they have no stored `hash` field — use `hash` and compare externally to whichever `body_hash` references the body).
- [x] Verify Ed25519 signatures on *Nova Locutio* messages (`verify` runs hash + signature for messages; `sign --seed <s>` produces deterministically-keyed signed messages).
- [x] Well-formedness checks for type expressions beyond JSON Schema (`check-type` subcommand): type-variable scoping, rank-1 polymorphism, uniqueness within sums and records, ctor-kind compatibility in `apply`.
- [x] Well-formedness checks for predicate, value, and body expressions (`check-predicate`, `check-value`, `check-body` subcommands): predicate op arity, value record field uniqueness, body lambda param uniqueness, and literal value soundness.
- [x] In-crate test suite (`cargo test`, 119 tests) covering canonicalization, hashing, kind detection, signing/verification, type well-formedness, predicate/value/body well-formedness, schema validation, and cross-file `$ref` resolution.
- [x] Cross-file `$ref` resolution: schemas may reference sibling schemas by their `https://novae-linguae.org/spec/...` identifier; `validate` resolves these against the local `spec/` tree (`validate_with_refs`). Used by the message schema for conditional `store`-payload validation.
- [x] Language-neutral conformance **vectors** (record → canonical bytes → hash, plus signing, signature, type well-formedness, and schema cases) exported as portable fixtures under [`spec/conformance/`](../../spec/conformance/) for cross-implementation byte-equality testing. The reference implementation replays them via `cargo test --test conformance`.

## Build

Requires a recent Rust toolchain (1.75+ recommended).

```bash
cd tooling/validator
cargo build --release
```

The compiled binary is at `target/release/nl-validator`.

## Tests

```bash
cd tooling/validator
cargo test
```

The suite lives under `tests/` and runs against the real artifacts in `spec/examples/` (resolved relative to the crate, so it works from any directory):

- `conformance.rs` — replays every vector in [`spec/conformance/manifest.json`](../../spec/conformance/manifest.json): canonical-byte and hash reproduction, the `body_hash` cross-reference, deterministic signing, signature verification, type well-formedness, and schema validation. The manifest — not Rust source — is the source of truth for the expected values.
- `ref_resolution.rs` — cross-file `$ref` resolution: a `store` message's payload validates against the referenced function-record schema; an invalid or kind-mismatched payload is rejected (proving the referenced schema is actually applied); relative and absolute logical refs resolve to files; unresolvable and out-of-namespace refs error without network access.
- `canonicalization.rs` — JCS key ordering, whitespace, idempotence, and source-key-order independence.
- `artifact_kind.rs` — auto-detection of function records / messages / body expressions and the field-stripping rules.
- `signing.rs` — deterministic key derivation reproduces the example messages byte-for-byte; DID and signature round-trips; tampering with a signed body or a record's contents is detected.
- `check_type.rs` — type well-formedness positives and negatives (unbound vars, nested `forall`, duplicate fields/tags, non-constructor `apply.ctor`).
- `schema_validation.rs` — every example validates against its schema; unknown fields, missing required fields, out-of-vocabulary speech acts, and wrong-typed fields are rejected.

The canonical-byte fixtures (`spec/conformance/canonical/*.jcs`) are regenerated with `cargo run --example gen_conformance`. If a golden hash changes after a deliberate edit to an example, regenerate the fixtures and update the affected `expected_hash` (and, for messages, `expected_signature`) in the manifest.

## Usage

All subcommands exit 0 on success, non-zero on failure. Validation diagnostics go to stderr with a JSON Pointer to the failing location.

### validate — JSON Schema structural check

```bash
# Function records
target/release/nl-validator validate \
    ../../spec/function-record.schema.json \
    ../../spec/examples/map.json

target/release/nl-validator validate \
    ../../spec/function-record.v0.2.schema.json \
    ../../spec/examples/map.v0.2.json

# Messages — v0.1 (string claim/commitment)
target/release/nl-validator validate \
    ../../spec/message.schema.json \
    ../../spec/examples/request.json

target/release/nl-validator validate \
    ../../spec/message.schema.json \
    ../../spec/examples/assert.json

# Messages — v0.2 (structured claim/commitment ASTs)
target/release/nl-validator validate \
    ../../spec/message.v0.2.schema.json \
    ../../spec/examples/assert.v0.2.json

target/release/nl-validator validate \
    ../../spec/message.v0.2.schema.json \
    ../../spec/examples/commit.v0.2.json

# Sub-language expressions
target/release/nl-validator validate \
    ../../spec/type-expression.schema.json \
    ../../spec/examples/type-map.json

target/release/nl-validator validate \
    ../../spec/predicate-expression.schema.json \
    ../../spec/examples/predicate-identity.json

target/release/nl-validator validate \
    ../../spec/value-expression.schema.json \
    ../../spec/examples/value-list-int.json

target/release/nl-validator validate \
    ../../spec/body-expression.schema.json \
    ../../spec/examples/body-double.json
```

**Cross-file `$ref` resolution.** When a schema references another schema by its logical identifier (`https://novae-linguae.org/spec/<version>/<file>`), `validate` resolves it to the file `<file>` in the schema's own directory — the version path segment is logical only; all schema files live flat in `spec/`. The message schema uses this to validate a `store` request's `payload` against the appropriate artifact schema, selected by the body's `payload_kind`:

```bash
# `store` payload is validated against function-record.schema.json via $ref
target/release/nl-validator validate \
    ../../spec/message.schema.json \
    ../../spec/examples/store-request.json
```

Non-`novae-linguae` reference URIs are refused (never fetched over the network). Schemas with only same-document (`#/...`) references behave exactly as before.

### hash — compute content-address

Auto-detects artifact kind from the record's top-level fields (message → `msg_`, body expression → `expr_`, function record → `fn_`). Pass `--kind` to override.

```bash
target/release/nl-validator hash ../../spec/examples/map.json
target/release/nl-validator hash ../../spec/examples/request.json
target/release/nl-validator hash --kind body ../../spec/examples/body-double.json
```

### verify — hash + signature check

For function records: verifies the stored `hash` field matches the computed hash.
For messages: verifies hash **and** Ed25519 signature.
Body expressions have no stored `hash`; use `hash` and compare against `body_hash` manually.

```bash
target/release/nl-validator verify ../../spec/examples/map.json
target/release/nl-validator verify ../../spec/examples/request.json
target/release/nl-validator verify ../../spec/examples/assert.json
target/release/nl-validator verify ../../spec/examples/assert.v0.2.json
target/release/nl-validator verify ../../spec/examples/commit.v0.2.json
```

### sign — produce a signed message

Signs a message in-place (or writes to stdout). Derives the Ed25519 key deterministically from the seed via BLAKE3(seed); sets `from` to `did:nova:<pubkey>`, recomputes `hash`, then signs.

```bash
# Print signed message to stdout
target/release/nl-validator sign --seed my-seed ../../spec/examples/request.json

# Overwrite file in place
target/release/nl-validator sign --seed my-seed --in-place ../../spec/examples/request.json
```

### check-type — type-expression well-formedness

Checks beyond JSON Schema: type-variable scoping, rank-1 polymorphism, uniqueness within sums and records, constructor-kind compatibility.

```bash
target/release/nl-validator check-type ../../spec/examples/type-map.json
```

### check-predicate — predicate-expression well-formedness

Checks arity of known built-in operators (`not/1`, `and/2`, `or/2`, `eq/2`, `foldl/3`, …). Unknown ops (content-address refs, scope variables) are not checked.

```bash
target/release/nl-validator check-predicate ../../spec/examples/predicate-identity.json
```

### check-value — value-expression well-formedness

Checks record field name uniqueness (not expressible in JSON Schema).

```bash
target/release/nl-validator check-value ../../spec/examples/value-list-int.json
```

### check-body — body-expression well-formedness

Checks lambda parameter name uniqueness and that `lit.value` is a well-formed value expression.

```bash
target/release/nl-validator check-body ../../spec/examples/body-double.json
target/release/nl-validator check-body ../../spec/examples/body-is-zero.json
```

### canonicalize — JCS canonical bytes

Writes the JCS-canonical form of a record to stdout (no trailing newline). Useful for debugging or piping into a hasher.

```bash
target/release/nl-validator canonicalize ../../spec/examples/map.json | xxd | head
```

## Why Rust

For a reference implementation:

- Fast; produces a single static binary via `cargo build --release`.
- Mature, well-maintained crates for everything we need: `jsonschema`, `serde_jcs`, `blake3`, `ed25519-dalek`.
- Aligned with the eventual ingestion-from-Rust path (the first ingestion adapter target is Rust crates).

Other implementations are welcome (Deno/TypeScript, Python, Go). All implementations MUST agree byte-for-byte on canonical form and hash. The conformance vectors at [`spec/conformance/`](../../spec/conformance/) pin this contract.

## Crate version notes

The `jsonschema` crate has gone through API changes over recent versions. This crate currently pins `jsonschema = "0.28"` and uses two call sites in `src/lib.rs`: `jsonschema::draft202012::new` for same-document validation, and `jsonschema::options().with_retriever(..).build(..)` (with the re-exported `Retrieve`/`Uri` types) for cross-file `$ref` resolution. If you upgrade the dependency, expect to adjust both.

## License

Dual-licensed under Apache-2.0 OR MIT, same as the rest of *Novae Linguae*.
