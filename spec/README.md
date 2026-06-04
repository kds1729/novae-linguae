# `spec/` — Novae Linguae specifications

This directory holds the machine-readable specifications for *Novae Linguae*. Schemas are the source of truth; the project's `README.md` describes intent, but anything binding lives here.

## Current contents

| Path | Status | What it defines |
|------|--------|------|
| `function-record.schema.json` | v0.1 draft | The function-record schema, v0.1 (string surface form for type / predicate / value fields) |
| `function-record.v0.2.schema.json` | v0.2 draft | The function-record schema, v0.2 — same shape as v0.1 but with structured type / predicate / value ASTs (sub-schemas inlined under `$defs` for self-containment) |
| `message.schema.json` | v0.1 draft | The structured speech-act envelope for *Nova Locutio* messages |
| `type-expression.schema.json` | v0.1 draft | Structured AST for *Nova Lingua* type expressions (used inline by `function-record.v0.2.schema.json`) |
| `predicate-expression.schema.json` | v0.1 draft | Structured AST for refinement predicates and property tests (used inline by `function-record.v0.2.schema.json`) |
| `value-expression.schema.json` | v0.1 draft | Structured AST for values in `examples.args` / `examples.result` (used inline by `function-record.v0.2.schema.json`) |
| `body-expression.schema.json` | v0.1 draft | Structured AST for the executable body that a function record's `body_hash` points to (seven expression kinds, four pattern kinds) |
| `canonical-serialization.md` | v0.1 | Normative spec for canonical form (JCS RFC 8785) and hashing (BLAKE3-256) |
| `trust-model.md` | v0.1 | Normative spec for the trust model: local trust policy + capability tokens + attestations, no central authority. Built on already-shipped *Nova Locutio* primitives. |
| `intent-tag-vocabulary.md` | v0.1 | Controlled vocabulary for `intent_tags`: 16 top-level categories (`transform`, `predicate`, `aggregate`, `filter`, `query`, `parse`, `serialize`, `io`, `arithmetic`, `math`, `logical`, `string`, `concurrent`, `crypto`, `time`, `coll`) plus property-modifier tags (`pure`, `elementwise`, `idempotent`, …). Non-vocab tags still validate; cross-agent agreement is the benefit. |
| `examples/map.json` | example | Concrete v0.1 function record for `map` (string surface form for type / predicate / value fields) |
| `examples/map.v0.2.json` | example | Concrete v0.2 function record for `map` (structured ASTs throughout); `supersedes` points at the v0.1 record |
| `examples/double.v0.2.json` | example | Concrete v0.2 function record for `double` (a `nat -> nat` function); referenced by `map.v0.2.json`'s `examples.args[].fn_ref` |
| `examples/type-map.json` | example | The type of `map` (`forall a b. (a -> b) -> List a -> List b`) as a standalone structured type-expression AST |
| `examples/predicate-identity.json` | example | The identity property of `map` as a standalone structured predicate AST |
| `examples/value-list-int.json` | example | The list `[1, 2, 3]` of natural numbers as a standalone structured value AST |
| `examples/body-double.json` | example | The body of `double` as a structured body-expression AST: `\n -> add(n, n)` |
| `examples/body-is-zero.json` | example | A `case`-using body: `\n -> case n of 0 -> True; _ -> False` (exercises pattern matching and literal/wildcard patterns) |
| `examples/request.json` | example | Concrete `request` message (apply `map` to `[1,2,3]`); signed with deterministic seed `novae-linguae-example-claude` |
| `examples/assert.json` | example | Concrete `assert` message claiming an identity property; signed with deterministic seed `novae-linguae-example-verifier` |

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
- Structured AST for *Nova Lingua* type expressions (rank-1 polymorphism, no kinds)
- Structured AST for predicate expressions (refinements + property tests)
- Structured AST for value expressions
- Structured AST for body expressions (the executable form behind `body_hash`)
- Closed speech-act vocabulary (nine acts: request, assert, query, propose, commit, retract, delegate, ack, reject)
- Closed effect vocabulary (ten effects, deliberately minimal)
- Closed reject-code vocabulary (six codes)
- Closed type-builtin vocabulary (eight atoms, five constructors)
- Open capability token format (`cap:path/segment`)
- Content-address format (`<kind>_<64-hex-blake3>`)
- Canonical form (JCS) and hash (BLAKE3-256) defined in [`canonical-serialization.md`](canonical-serialization.md)
- DID-based agent identity (`did:nova:<64-hex-pubkey>` in v0.1), Ed25519 signing
- Trust model (local policy + capability tokens + attestations, no central authority) defined in [`trust-model.md`](trust-model.md)
- Strict `additionalProperties: false` everywhere — unknown fields fail validation

**Well-formedness vs structural validation.** JSON Schema can only check shape. Several semantic constraints are real but live outside the schema; they are enforced by the reference validator at [`tooling/validator/`](../tooling/validator/). Today this covers type-expression well-formedness (variable scoping, rank-1 forall, uniqueness within sums and records, ctor-kind compatibility in `apply`). Predicate- and value-expression well-formedness will follow once the verifier engine engages with them.

## What v0.1 deliberately defers

These are real specifications that will arrive in their own schemas. v0.1 stringifies them so we can start populating the commons without blocking on the full design.

1. **Type expression sub-language.** v0.1 `signature.type` in `function-record.schema.json` is still a string in surface syntax. **RESOLVED** at the schema layer in [`type-expression.schema.json`](type-expression.schema.json) (nine kinds: `var`, `ref`, `builtin`, `forall`, `fn`, `apply`, `tuple`, `record`, `sum`) with well-formedness checks in `nl-validator check-type`; **made mandatory by [`function-record.v0.2.schema.json`](function-record.v0.2.schema.json)**. Deferred to a later version of the type-expression schema itself: kind annotations, higher-rank polymorphism, type classes / traits, linear/affine types, existential types, GADTs, type-level lambdas.
2. **Refinement / predicate expression sub-language.** v0.1 `signature.refinements[].expr` in `function-record.schema.json` is still a string in surface syntax. **RESOLVED** at the schema layer in [`predicate-expression.schema.json`](predicate-expression.schema.json), and **made mandatory by [`function-record.v0.2.schema.json`](function-record.v0.2.schema.json)**. New records SHOULD target v0.2; v0.1 records remain valid against their pinned schema.
3. **Property expression sub-language.** v0.1 `properties[].expr` is still a string. **RESOLVED** — shares `predicate-expression.schema.json` with refinements; made mandatory by `function-record.v0.2.schema.json`.
4. **Value representation in examples.** v0.1 allows any JSON value for `args` and `result`, with a bare-string-for-function-references convention. **RESOLVED** in [`value-expression.schema.json`](value-expression.schema.json) (eleven kinds: `bool`, `int`, `nat`, `float`, `string`, `bytes`, `unit`, `list`, `tuple`, `record`, `variant`, `fn_ref`); made mandatory by `function-record.v0.2.schema.json`.
5. **Body representation.** v0.1 references the body by hash (`body_hash`) but does not specify the body's structure. **RESOLVED** end-to-end in [`body-expression.schema.json`](body-expression.schema.json): structured AST with seven expression kinds (`var`, `lit`, `app`, `let`, `lambda`, `case`, `field`) and four pattern kinds (`wildcard`, `bind`, `variant`, `lit`). Embedded types and values are accepted as opaque objects at this layer and must validate independently against `type-expression.schema.json` and `value-expression.schema.json`. The reference validator `nl-validator hash` auto-detects body expressions from the top-level `kind` field; `--kind body` is available as an override. Example function records (`double.v0.2.json`) now point at the real `expr_<…>` content-address of their body (`body-double.json`), and `verify` confirms the chain end-to-end. Deferred to a later body-expression schema version: optional type annotations on `let`, multi-binding `let`, multi-arm lambda equivalence sugar, do-notation, effect rows.
6. **Canonical serialization for hashing.** ~~v0.1 mentions canonical serialization but does not define it.~~ **RESOLVED in [`canonical-serialization.md`](canonical-serialization.md)**: JCS (RFC 8785) over UTF-8 JSON, BLAKE3-256 as the hash. The reference validator/hasher at [`tooling/validator/`](../tooling/validator/) implements the procedure end-to-end; example records now carry real, reproducible hashes that `nl-validator verify` passes.
7. **Controlled intent-tag vocabulary.** **RESOLVED** in [`intent-tag-vocabulary.md`](intent-tag-vocabulary.md): sixteen top-level categories and a set of property-modifier tags, with an extension policy. The schema continues to accept any tag matching the path pattern; the vocabulary is the convention for cross-agent agreement.
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

All three example records (`map.json`, `request.json`, `assert.json`) carry real hashes and (for messages) real Ed25519 signatures, reproducible via `nl-validator hash` / `nl-validator sign --seed <s>`. They PASS `nl-validator verify`.

## Validating a record or message

The reference validator at [`tooling/validator/`](../tooling/validator/) provides the full check (JSON Schema + canonicalization + hash + signature). Build with `cargo build --release` from `tooling/validator/`; then from the repo root:

```bash
# Structural (JSON Schema) validation
./tooling/validator/target/release/nl-validator validate spec/function-record.schema.json spec/examples/map.json
./tooling/validator/target/release/nl-validator validate spec/message.schema.json         spec/examples/request.json
./tooling/validator/target/release/nl-validator validate spec/message.schema.json         spec/examples/assert.json
./tooling/validator/target/release/nl-validator validate spec/type-expression.schema.json spec/examples/type-map.json
./tooling/validator/target/release/nl-validator validate spec/predicate-expression.schema.json spec/examples/predicate-identity.json
./tooling/validator/target/release/nl-validator validate spec/value-expression.schema.json spec/examples/value-list-int.json

# End-to-end hash + signature verify (messages get both)
./tooling/validator/target/release/nl-validator verify spec/examples/map.json
./tooling/validator/target/release/nl-validator verify spec/examples/request.json
./tooling/validator/target/release/nl-validator verify spec/examples/assert.json

# Type-expression well-formedness (beyond JSON Schema)
./tooling/validator/target/release/nl-validator check-type spec/examples/type-map.json
```

Any JSON Schema 2020-12 validator can also be used for structural checks; the reference is byte-equality of hash and JCS form across implementations.

## Contributing

Schema changes go through the same review as any other change. **Schema changes that affect record interpretation must bump the version**, even if the JSON Schema would still validate prior records — semantic compatibility is stricter than syntactic compatibility.
