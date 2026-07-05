# Canonical serialization (v0.1)

## Purpose

This document defines exactly what "canonical serialization" means in *Novae Linguae*. Both the function-record schema and the message schema reference it: the `hash` field on each artifact is the BLAKE3-256 of the canonical serialization of the artifact with a defined set of fields removed; on messages, the `signature` field signs canonical-serialized bytes.

Without a single normative canonical form, two correctly-implemented agents producing the same logical record disagree on the hash. The whole content-addressed-commons premise (principle 2 in the project README) requires that two implementations agree byte-for-byte on what gets hashed. This document settles that.

## Choice: JSON Canonicalization Scheme (JCS), RFC 8785

*Novae Linguae* v0.1 uses **[JCS as defined in RFC 8785](https://www.rfc-editor.org/rfc/rfc8785)** applied to UTF-8 JSON.

### Why JCS and not deterministic CBOR

| Property | JCS (RFC 8785) | Deterministic CBOR (RFC 8949 §4.2) |
|---|---|---|
| Output is human-readable | Yes — JSON text | No — binary |
| AI authors today produce this format fluently | Yes | Less so |
| Toolchain maturity | Mature | Mature |
| Wire efficiency | Lower | Higher |
| Big-integer native | No (workaround required) | Yes |
| Floating point precision | IEEE 754 doubles | IEEE 754 doubles |

The deciding factor is principle 4 (the author is an AI agent) combined with current model fluency: today's frontier LLMs produce JSON fluently and CBOR clumsily. Records in *Novae Linguae* are authored by AI; that author's fluency dominates this layer of the design.

The wire-efficiency advantage of CBOR matters at the **network transport** layer, not at the **canonical hashing** layer, and is recoverable later — a deterministic JSON↔CBOR mapping exists and can be specified for Nova Locutio transport without changing the canonical-hash bytes.

### What v0.2+ may revisit

- A deterministic CBOR encoding for Nova Locutio wire transport (the canonical hash would still be computed over JCS, so hashes remain stable across encodings).
- A native big-integer extension if real *Nova Lingua* values require integers beyond `2^53 − 1`.

## The rules

[RFC 8785](https://www.rfc-editor.org/rfc/rfc8785) is normative. The summary below is for orientation; if this document and the RFC disagree, the RFC wins.

1. **Encoding** — UTF-8 with no byte-order mark.
2. **Object members** — sorted lexicographically by member name. Sort order is on the UTF-16 code-unit representation of the name string (ECMAScript / JCS rule).
3. **No insignificant whitespace** — no whitespace between any tokens. No trailing newline.
4. **Numbers** — serialized per ECMAScript `Number.prototype.toString` then conditioned per JCS §3.2.2.3. Implementations should call a JCS library; hand-rolling number serialization is the most common source of cross-implementation drift.
5. **Strings** — escape only the characters JSON requires (`"`, `\`, and `U+0000`–`U+001F`). Use the shortest valid escape (`\n`, `\t`, `\r`, etc.) where one is defined; otherwise `\u00XX`.
6. **Arrays** — element order preserved.
7. **Booleans and null** — exactly `true`, `false`, `null`.
8. **No literal `NaN`, `+Infinity`, `-Infinity`** — JSON does not permit them and JCS therefore forbids them. Encode such values as strings, or refactor to avoid them.
9. **No negative zero** — `-0` is serialized as `0`.

## Novae Linguae extensions and clarifications

These are clarifications on top of JCS, not departures from it.

1. **Integers beyond `2^53 − 1` are encoded as strings of decimal digits**, never as JSON numbers. Content-addresses (`fn_<hex>`, `msg_<hex>`, etc.) are already strings, so this clarification only affects user-supplied values in fields like `examples.args` and `examples.result`. The value sub-language (a deferred item) will formalize this once it exists.
2. **No defaults are materialized before hashing.** Schema-declared defaults are *not* substituted into the record. An omitted field with a schema default is hashed as if the field is absent; two implementations therefore agree on the canonical form iff they agree on which fields are *present*, not which fields are *meaningful*.
3. **Validation precedes hashing.** Every record and message schema declares `additionalProperties: false`. A record that does not validate against its schema MUST NOT be hashed; hashes of invalid records are undefined.

## Hash algorithm

**BLAKE3-256.** Output is 32 bytes (256 bits); render as 64 lowercase hexadecimal characters when embedded in records.

Rationale:
- Modern, fast, parallelizable.
- Well-specified ([official spec](https://github.com/BLAKE3-team/BLAKE3-specs)), widely implemented.
- A single fixed output size sufficient for content addressing.
- Drop-in replacement for SHA-256 with substantially better performance.

Multi-algorithm support (multihash-style prefix tagging) is deferred. v0.1 records and messages all use BLAKE3-256; any future migration is a major version bump and would extend the address prefix vocabulary.

## What gets hashed, and what gets signed

### Function records

To compute the `hash` field of a function record:

1. Start with the record as a JSON object that validates against `function-record.schema.json`.
2. Remove the `hash` field. (It is being computed; including it would be circular.)
3. JCS-canonicalize the result per the rules above.
4. BLAKE3-256 the resulting UTF-8 bytes.
5. Render as `fn_` followed by 64 lowercase hex characters.

The `body_hash` field is computed separately, over a different artifact (the expression body), and is addressed as `expr_<…>`. It does not participate in the function-record hash computation other than being one of the bytes covered by it.

### Nova Locutio messages

To compute the message `hash`:

1. Start with the message as a JSON object that validates against `message.schema.json`.
2. Remove **both** the `hash` field and the `signature` field.
3. JCS-canonicalize.
4. BLAKE3-256.
5. Render as `msg_` followed by 64 lowercase hex characters.

To compute the message `signature`:

1. Start with the same JSON object, with the `hash` field already filled in from the step above.
2. Remove **only** the `signature` field. (The `hash` field is included in what is signed — tampering with the hash is therefore detectable.)
3. JCS-canonicalize.
4. Sign the resulting UTF-8 bytes with Ed25519 using the sender's private key.
5. Render the 64-byte signature as base64 and prefix with `ed25519:`.

The authoring sequence for a message is therefore:

1. Fill in every field *except* `hash` and `signature`.
2. Compute and write `hash`.
3. Compute and write `signature`.

The verification sequence is:

1. Remove `signature`, JCS-canonicalize, Ed25519-verify with the public key resolved from the `from` DID.
2. Remove `hash` and `signature`, JCS-canonicalize, BLAKE3-256, compare against the stored `hash`.
3. Both checks must pass.

### Certification records

A **certification** (top-level `kind: "certification"`, produced by `nl-validator certify --sign`) is a
certifier's signed attestation that a function record passed every "verified by default" check. It is hashed
and signed by **exactly the same rules as a message** — remove `hash` and `signature` before hashing; remove
only `signature` before signing (so the signature covers the hash); resolve the signer's key from the `from`
DID on verification — the only difference being the address prefix: a certification's `hash` is rendered as
`cert_` followed by 64 lowercase hex characters. Its `subject` (a `fn_…` address) and `body_hash` (an
`expr_…` address) name what was certified, so a certification is itself a content-addressed, tamper-evident
commons artifact that other agents can rely on when assembling.

### Weights records

A **weights pointer record** (top-level `kind: "weights"`, [`weights.schema.json`](weights.schema.json) /
[`weights.md`](weights.md)) is hashed by **exactly the same rules as a function record** — remove `hash`,
JCS-canonicalize, BLAKE3-256 — with the address prefix `wgt_`. It is **unsigned**: provenance and measured
capability are carried by signed eval attestations *about* its address, never by the pointer itself. Note
the two hash layers: the record's *address* is BLAKE3 over its canonical JSON as everywhere in the commons,
while the `files[].sha256` entries inside it identify the **binary blobs** the record points at (sha256 by
ML-ecosystem convention — blobs are not JSON and never enter the JCS pipeline).

### Eval attestations

An **eval attestation** (top-level `kind: "eval-attestation"`, produced by `nl-validator attest-weights
--sign`, [`eval-attestation.schema.json`](eval-attestation.schema.json)) is hashed and signed by **exactly
the same rules as a message** — remove `hash` and `signature` before hashing; remove only `signature` before
signing; resolve the signer's key from the `from` DID — with the address prefix `evl_`. Its `subject` (a
`wgt_…` address) names the weights whose measured capability is attested.

## Worked example

Given this minimal function record:

```json
{
  "schema_version": "0.1.0",
  "name_hints": ["map"],
  "signature": {
    "type": "forall a b. (a -> b) -> List a -> List b",
    "effects": [],
    "capabilities": [],
    "terminates": "conditional"
  },
  "examples": [{ "args": ["double", [1, 2, 3]], "result": [2, 4, 6] }],
  "body_hash": "expr_8f2c7d6e5b4a392817160f0e0d0c0b0a09080706050403020100ffeeddccbbaa"
}
```

The JCS canonical form is the following single line of UTF-8 (line-broken here only so it fits the page; the real form has no whitespace and no line breaks):

```
{"body_hash":"expr_8f2c7d6e5b4a392817160f0e0d0c0b0a09080706050403020100ffeeddccbbaa",
"examples":[{"args":["double",[1,2,3]],"result":[2,4,6]}],
"name_hints":["map"],
"schema_version":"0.1.0",
"signature":{"capabilities":[],"effects":[],"terminates":"conditional","type":"forall a b. (a -> b) -> List a -> List b"}}
```

Observations:

- Top-level keys appear alphabetically: `body_hash`, `examples`, `name_hints`, `schema_version`, `signature`.
- Inside `signature`, keys appear alphabetically: `capabilities`, `effects`, `terminates`, `type`.
- Inside `examples[0]`, keys appear alphabetically: `args`, `result`.
- Arrays (`examples`, `args`, `result`, the inner number list) preserve their authored order.
- There is no whitespace between any tokens.

BLAKE3-256 over the exact UTF-8 bytes of that single line yields the digest; rendered as `fn_<64 lowercase hex>`, it is the record's `hash` field.

## Implementation notes

- **JCS libraries**: `json-canonicalization` (Python), `jcs` (JavaScript/TypeScript), `serde_jcs` (Rust). Do not hand-roll number serialization.
- **BLAKE3 libraries**: `blake3` (Python, Rust, JavaScript, Go); CLI tool `b3sum`. Accept arbitrary byte input.
- **Validation order**: validate records against the JSON Schema *before* canonicalizing. The schema's `additionalProperties: false` rule means an invalid record cannot produce a meaningful hash.
- **Test vectors**: a reference test suite (record → canonical bytes → BLAKE3 → hash) should be published once the validator exists, so any implementation can verify it agrees with the spec.

## Open questions

Tracked for v0.2+, not blockers for v0.1.

1. **Big-integer canonicalization.** v0.1 uses decimal-digit strings. v0.2+ should formalize this via the value sub-language, and may revisit if a use case requires native big integers (which would push toward CBOR for that field).
2. **Cross-encoding determinism.** If CBOR is adopted for Nova Locutio transport, the JSON↔CBOR mapping must be normatively specified so canonical hashes remain stable across encodings.
3. **Multi-algorithm hashing.** If BLAKE3-256 ever needs migration, multihash-style prefix tagging (`fn_blake3-<hex>` / `fn_sha3-<hex>`) is the migration path. Requires a major version bump.
4. **Schema-evolution interaction.** When a record's schema bumps from v0.1.0 to v0.2.0, canonical forms (and therefore hashes) of the same logical record may differ. The `schema_version` field anchors interpretation; hashes are not portable across schema major versions. The commons should treat the same logical record at different schema versions as different artifacts at different hashes, linked by `supersedes`/`derived_from` metadata.
