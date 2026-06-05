# `spec/conformance/` — cross-implementation conformance vectors

These fixtures pin the byte-level behavior every *Novae Linguae* implementation must agree on. They are **language-neutral**: the contract lives in [`manifest.json`](manifest.json) and the canonical-byte fixtures under [`canonical/`](canonical/), not in any one implementation's source. The reference Rust validator is simply the first consumer — it replays every vector in its test suite (`tooling/validator/tests/conformance.rs`).

If you are writing a second implementation (Deno/TypeScript, Python, Go, …), you conform iff you reproduce every vector here.

## Layout

```
conformance/
  manifest.json        the contract: all vectors, with expected results
  canonical/*.jcs      golden JCS canonical preimages (the exact bytes that get hashed)
  README.md            this file
```

## How to read the manifest

`manifest.json` groups vectors into sections. All paths are **relative to the manifest file** (so `../examples/map.json` is `spec/examples/map.json`). Each vector supplies its input as either:

- `input` (or `record` / `schema`) — a path to a JSON file, or
- `input_inline` — an embedded JSON value (used for small synthetic cases).

Read whichever is present.

### Sections

| Section | What an implementation must do |
|---|---|
| `hash_vectors` | Strip `stripped_fields`, JCS-canonicalize (RFC 8785), BLAKE3-256, prefix. The result must equal `expected_hash`, **and** the canonical bytes must equal the file at `canonical_preimage` byte-for-byte. |
| `cross_reference_vectors` | A record's `field` (e.g. `body_hash`) must equal the `expected_hash` of the named hash vector. |
| `signing_vectors` | Derive the key as BLAKE3-256(`seed`), set `from`, recompute `hash`, sign. Must reproduce `expected_from`, `expected_hash`, `expected_signature` exactly. |
| `signature_verification_vectors` | `valid` inputs must verify; `invalid` must not. |
| `type_wellformedness_vectors` | `well-formed` inputs must pass the type checker; `ill-formed` must fail. |
| `schema_validation_vectors` | `valid` instances must validate against the named schema (JSON Schema draft 2020-12); `invalid` must fail. |

## Why both canonical bytes *and* hashes

A matching hash already proves the preimage matched (BLAKE3 is collision-resistant). The `.jcs` files exist for **diagnosis**: when an implementation's hash differs, comparing its canonical bytes against the `.jcs` fixture tells you immediately whether the bug is in canonicalization (bytes differ) or in hashing (bytes match, hash differs). The `.jcs` files have no trailing newline — they are exact preimages.

## Regenerating the canonical fixtures

The `.jcs` files are generated from the example artifacts; the manifest and this README are hand-maintained. To regenerate after a deliberate change to an example:

```bash
cd tooling/validator
cargo run --example gen_conformance
```

Then update the affected `expected_hash` (and, for messages, `expected_signature`) in `manifest.json`. The conformance test will fail loudly until the manifest matches the regenerated fixtures.

## Running the vectors (reference implementation)

```bash
cd tooling/validator
cargo test --test conformance
```
