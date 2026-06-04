# `spec/` — Novae Linguae specifications

This directory holds the machine-readable specifications for *Novae Linguae*. Schemas are the source of truth; the project's `README.md` describes intent, but anything binding lives here.

## Current contents

| Path | Status | What it defines |
|------|--------|------|
| `function-record.schema.json` | v0.1 draft | The mandatory metadata record for every function in *Nova Lingua* |
| `message.schema.json` | v0.1 draft | The structured speech-act envelope for *Nova Locutio* messages |
| `canonical-serialization.md` | v0.1 | Normative spec for canonical form (JCS RFC 8785) and hashing (BLAKE3-256) |
| `examples/map.json` | example | Concrete function record for `map` |
| `examples/request.json` | example | Concrete `request` message (apply `double` to `[1,2,3]`) |
| `examples/assert.json` | example | Concrete `assert` message claiming an identity property |

## Versioning policy

- **Semantic versioning** on each schema.
- `schema_version` is **pinned into every record**, not stored once project-wide. Old records remain readable forever; consumers branch on version.
- **Patch bumps** (0.1.0 → 0.1.1): documentation only, no structural change.
- **Minor bumps** (0.1.x → 0.2.0): additive — new optional fields, new enum values where the field is documented as open, broader patterns. Existing records remain valid.
- **Major bumps** (0.x → 1.0): breaking. A migration path must accompany the bump; previous-version records remain valid against their pinned schema.
- The schema's own evolution is content-addressed in the same way records are: the schema file has a hash, and that hash is what records actually conform to.

## What v0.1 includes

- Full structural shape of a function record
- Full structural shape of a *Nova Locutio* message envelope
- Closed speech-act vocabulary (nine acts: request, assert, query, propose, commit, retract, delegate, ack, reject)
- Closed effect vocabulary (ten effects, deliberately minimal)
- Closed reject-code vocabulary (six codes)
- Open capability token format (`cap:path/segment`)
- Content-address format (`<kind>_<64-hex-blake3>`)
- DID-based agent identity, Ed25519 signing
- Strict `additionalProperties: false` everywhere — unknown fields fail validation

## What v0.1 deliberately defers

These are real specifications that will arrive in their own schemas. v0.1 stringifies them so we can start populating the commons without blocking on the full design.

1. **Type expression sub-language.** v0.1 `signature.type` is a string in surface syntax. v0.2+ will define a structured type AST (type variables, foralls, type constructors, kinds).
2. **Refinement / predicate expression sub-language.** v0.1 stringifies refinement predicates. v0.2+ will define a structured predicate AST evaluable by the verification engine.
3. **Property expression sub-language.** Same — v0.1 stringifies; v0.2+ will define an AST executable by a property-based testing engine.
4. **Value representation in examples.** v0.1 allows any JSON value for `args` and `result`. As a v0.1 convention, function references in argument positions are written as bare strings naming the function (e.g. `"double"`) — informal and ambiguous with string literals, accepted only because the value sub-language is not yet defined. v0.2+ will define a canonical value representation including function references, opaque handles, and structured constants.
5. **Body representation.** v0.1 references the body by hash (`body_hash`) but does not specify the body's structure. The expression AST is its own spec.
6. **Canonical serialization for hashing.** ~~v0.1 mentions canonical serialization but does not define it.~~ **RESOLVED in [`canonical-serialization.md`](canonical-serialization.md)**: JCS (RFC 8785) over UTF-8 JSON, BLAKE3-256 as the hash. Hash values in existing example records remain placeholder hex because the reference validator/hasher that recomputes them does not yet exist; they will become reproducible once that tool lands.
7. **Controlled intent-tag vocabulary.** v0.1 allows any slash-separated lowercase tag. v0.2+ will publish a controlled vocabulary so two agents tag the same concept the same way.
8. **Claim and commitment expression sub-languages.** v0.1 stringifies `assert.claim` and `commit.commitment`. v0.2+ will define structured ASTs so receivers can mechanically verify what is being asserted or committed to.
9. **Multicast addressing.** v0.1 messages have a single receiver or null (broadcast). Multicast / group addressing deferred.
10. **Multi-algorithm signatures.** v0.1 fixes Ed25519. Multi-algorithm signing (multisig-style algorithm tagging) deferred.
11. **Absolute deadlines.** v0.1 `constraints.deadline_ms` is relative to receipt. Wall-clock and causal deadlines deferred.
12. **Cross-schema validation.** v0.1 `request.payload` (for `action: "store"`) is an unconstrained object. v0.2+ will conditionally validate the payload against the appropriate artifact schema.

## Content-address format (v0.1)

```
<kind>_<digest>

kind   ::= "fn" | "expr" | "type" | "proof" | "msg"
digest ::= 64 lowercase hex characters = 32 bytes = 256 bits, BLAKE3-256
```

Examples:
- `fn_3a9b…` — a function
- `expr_8f2c…` — an expression body
- `type_…` — a type
- `proof_…` — a verification certificate
- `msg_…` — a *Nova Locutio* message

The fixed algorithm (BLAKE3-256) and fixed encoding (lowercase hex) are deliberate. Multihash-style algorithm tagging is rejected for v0.1 because it adds variability before we need it. If we ever migrate algorithms, that is a major version bump and the prefix vocabulary expands.

## Hashing and signing semantics (v0.1)

Function records and messages both have a `hash` field that identifies the artifact globally. The full normative procedure is in **[`canonical-serialization.md`](canonical-serialization.md)**; the brief summary:

- **Function record hash**: BLAKE3-256 of the JCS-canonical serialization of the record with the `hash` field removed. Computed externally; the field is asserted, not validated, by the schema.
- **Message hash**: BLAKE3-256 of the JCS-canonical serialization of the message with the `hash` and `signature` fields removed.
- **Message signature**: Ed25519 over the JCS-canonical serialization of the message with only the `signature` field removed (so the `hash` is included in what is signed — tampering with the hash is detectable).

Hash values in current example records remain placeholder hex; they become reproducible once a reference validator/hasher exists.

## Validating a record or message

Until *Novae Linguae* tooling exists, any JSON Schema 2020-12 validator works. From the repo root:

```bash
# Example with ajv-cli (npm install -g ajv-cli ajv-formats)
ajv validate -s spec/function-record.schema.json -d spec/examples/map.json     --spec=draft2020
ajv validate -s spec/message.schema.json         -d spec/examples/request.json --spec=draft2020
ajv validate -s spec/message.schema.json         -d spec/examples/assert.json  --spec=draft2020
```

## Contributing

Schema changes go through the same review as any other change. **Schema changes that affect record interpretation must bump the version**, even if the JSON Schema would still validate prior records — semantic compatibility is stricter than syntactic compatibility.
