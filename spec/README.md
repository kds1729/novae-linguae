# `spec/` — Novae Linguae specifications

This directory holds the machine-readable specifications for *Novae Linguae*. Schemas are the source of truth; the project's `README.md` describes intent, but anything binding lives here.

## Current contents

| Path | Status | What it defines |
|------|--------|------|
| `function-record.schema.json` | v0.1 draft | The function-record schema, v0.1 (string surface form for type / predicate / value fields) |
| `function-record.v0.2.schema.json` | v0.2 draft | The function-record schema, v0.2 — same shape as v0.1 but with structured type / predicate / value ASTs (sub-schemas inlined under `$defs` for self-containment). Carries an optional v0.3 `signature.cost` (`time` / `measure` / `output_size`) enabling **precise** complexity composition through pipelines |
| `message.schema.json` | v0.1 draft | The structured speech-act envelope for *Nova Locutio* messages |
| `message.v0.2.schema.json` | v0.2 draft | *Nova Locutio* message envelope v0.2 — only breaking change: `assert_body.claim` and `commit_body.commitment` are now required structured ASTs (`claim-expression.schema.json` / `commitment-expression.schema.json`) instead of free-form strings |
| `type-expression.schema.json` | v0.1 draft | Structured AST for *Nova Lingua* type expressions (used inline by `function-record.v0.2.schema.json`) |
| `predicate-expression.schema.json` | v0.1 draft | Structured AST for refinement predicates and property tests (used inline by `function-record.v0.2.schema.json`) |
| `value-expression.schema.json` | v0.1 draft | Structured AST for values in `examples.args` / `examples.result` (used inline by `function-record.v0.2.schema.json`) |
| `body-expression.schema.json` | v0.1 draft | Structured AST for the executable body that a function record's `body_hash` points to (seven expression kinds, four pattern kinds) |
| `claim-expression.schema.json` | v0.1 draft | Structured AST for `assert.claim` (predicate / satisfies / verified / **attestation** — a trust verb about an agent or artifact: `vouches-for` / `trusts-claims-about` / `distrusts`) |
| `commitment-expression.schema.json` | v0.1 draft | Structured AST for `commit.commitment` (apply / provide / refrain) |
| `surface-syntax.md` | v0.1 | Concrete syntax for all four expression sub-languages (type, predicate, value, body): grammar, infix-to-AST mapping, canonical pretty-print rules, and round-trip requirement. Parser/pretty-printer shipped in `nl-validator` (`parse-*`/`unparse-*` subcommands), with round-trip conformance vectors for all four sub-languages |
| `evaluation.md` | v0.1 | Normative-by-reference spec for the semantic core: how a body **executes** (call-by-value, closures, currying, `case`, builtins incl. map/filter/fold/compose, `fn_ref` composition) and how it is **type-checked** (Hindley-Milner, skolemized `forall`), how effects are **enforced + statically inferred** (a capability sandbox over real-I/O builtins — `fs`/`net`/`process` — with record/replay), and how properties are checked **generatively / bounded-exhaustively** and **proved over the unbounded domain** — a first-order **SMT backend** (SMT-LIB 2 emission + re-checkable certificates) with a **structural-induction** fallback for list laws (base + step discharged via `define-fun-rec` over a `Lst` datatype), lemma discovery (curated catalog + theory exploration), induction over user-defined recursion and folds, **semantic-equivalence** proving (`equiv`), behavioral-equivalence **clustering** (`cluster`), **composite-metadata** derivation for pipelines (`compose`, multi-argument stages), and verification of a body against its **declared metadata** — the type-implied `nat` and declared `pre`/`post` **refinements** (`check-refinement`, the reserved `result` variable) a declared **`terminates: always`** by structural analysis (`check-termination`), and a declared **`complexity`** (`O(…)`) — with the structured **`cost`** (`time` + `output_size`) — by structural cost analysis (`check-complexity` — a recurrence over the first-order fragment, sound upper bound, no solver; `output_size` verification closes the soundness gap in `compose`'s precise-complexity path). All of these run together via **`certify`**, which emits a single re-checkable verification certificate. Implemented in `nl-validator` (`eval`/`run`/`typecheck`/`check-properties`/`check-effects`/`check-refinement`/`check-termination`/`check-complexity`/`certify`/`prove`/`equiv`/`cluster`/`compose`). Load-bearing for principles 2, 3, 5, and 9. |
| `agent-loop.md` | v0.2 | Normative-by-reference spec for the **Nova Locutio agent loop**: a responder consumes a signed `request` (`apply`), resolves + **runs** the target over its value-expression args (joining a Nova Locutio message to a Nova Lingua evaluation), and emits a signed `assert` whose `predicate` claim is the computed equation `eq(target(args…), result)`; any receiver **re-runs** the claim to confirm it (verification is re-execution — no privileged party). `respond` also answers `validate`/`query`/`propose`/`store`/`commit`/`delegate`/`retract` and **capability-gates** `apply`/`propose`; `orchestrate` drives a full multi-stage `query → propose → commit → assert → verify` pipeline autonomously, composing discovered functions; `orchestrate --verify` adds the trust+proof gates — discover → filter candidates by signature compatibility → trust-rank under the receiver's local policy → re-prove the chosen function's property → apply → re-verify. Implemented in `nl-validator` (`respond` / `orchestrate` / `verify-claim`). Load-bearing for principles 1, 3, 4, 6, 7. |
| `canonical-serialization.md` | v0.1 | Normative spec for canonical form (JCS RFC 8785) and hashing (BLAKE3-256) |
| `trust-model.md` | v0.1 | Normative spec for the trust model: local trust policy + capability tokens + attestations, no central authority. All three reference pieces now built in `nl-validator`: the capability verifier (`verify-delegation`), the attestation-graph query layer (`AttestationGraph`), and the policy engine (`evaluate-trust` / `authorize`). |
| `intent-tag-vocabulary.md` | v0.1 | Controlled vocabulary for `intent_tags`: 16 top-level categories (`transform`, `predicate`, `aggregate`, `filter`, `query`, `parse`, `serialize`, `io`, `arithmetic`, `math`, `logical`, `string`, `concurrent`, `crypto`, `time`, `coll`) plus property-modifier tags (`pure`, `elementwise`, `idempotent`, …). Non-vocab tags still validate; cross-agent agreement is the benefit. |
| `encrypted-envelope.schema.json` | v0.2 / v0.3 draft | The *Nova Locutio* encrypted-envelope schema: a multi-recipient sealed box (per-recipient key-wrap from `did:nova` keys, XChaCha20-Poly1305 AEAD, HKDF-SHA-256). A transport artifact — not content-addressed. `kex` selects X25519 (`x25519-ed25519`, v0.2) or the **post-quantum hybrid** (`x25519-mlkem768`, v0.3, adds a per-recipient `kem_ct`); optional `addressing: stealth` (v0.3) hides the recipient set. |
| `encryption.md` | v0.2 / v0.3 | Normative spec for payload encryption: identity reuse (Ed25519→X25519), the seal/open construction, algorithm rationale, security considerations, **stealth recipient addressing** (v0.3, implemented), and the **post-quantum hybrid `kex`** (v0.3 `x25519-mlkem768`, X25519 + ML-KEM-768, implemented). Load-bearing for principle 7. Reference impl at [`tooling/crypto-python/`](../tooling/crypto-python/) (incl. `ml_kem.py`); hardened impl at [`tooling/validator/src/seal.rs`](../tooling/validator/src/seal.rs) (`nl-seal`). |
| `crypto-conformance.md` | v0.2 / v0.3 | The binding contract: an encryption implementation is conformant iff it reproduces `conformance/encryption.json` byte for byte. Defines the construction, the RNG draw-order contract (incl. the hybrid ML-KEM `m` draw), and how to run the replay (`nl-seal conformance`); both the Python reference and the hardened Rust impl pass it, including the ML-KEM-768 KAT and `mlkem768_envelope` vectors. |
| `did-document.schema.json` | v0.3 draft | The DID-document schema: a self-verifying record that publishes a `did:nova` identity's ML-KEM-768 key-agreement key (which, unlike X25519, cannot be derived from the Ed25519 identity). Signed by the identity itself — no central authority. |
| `did-document.md` | v0.3 | Normative spec for DID documents: why they exist (publishing a post-quantum key the DID string cannot carry), the signed `{id, keyAgreement, signature}` shape, deterministic seed-derived key material, and resolution/trust. Prerequisite for `kex: x25519-mlkem768` ([`encryption.md`](encryption.md)). |
| `commons.md` | v0.2 | Normative spec for the commons: a content-addressed, self-verifying, federatable store + discovery protocol (publish / resolve / query / search / sync). Engine-agnostic — the store is untrusted infrastructure (clients verify by hash + signature), with no central authority (principle 7). Reference node (Django + Postgres/pgvector + Redis) is built and deployed — **Arca**, live at https://nl.1105software.com. |
| `resilience.md` | design | Forward-looking (not yet implemented) availability/anti-sabotage design for the public service **Arca**: the "transport is untrusted" property, seed bundles, the standard `.nlb` commons-bundle release-artifact format, and a pluggable censorship-resistant bootstrap (blockchain anchor / Nostr / IPNS / DNS). Deployment + cost-control counterpart at [`../tooling/commons-node/DEPLOYMENT.md`](../tooling/commons-node/DEPLOYMENT.md). |
| `examples/map.json` | example | Concrete v0.1 function record for `map` (string surface form for type / predicate / value fields) |
| `examples/map.v0.2.json` | example | Concrete v0.2 function record for `map` (structured ASTs throughout); `supersedes` points at the v0.1 record. `body_hash` resolves to the committed [`body-map.json`](examples/body-map.json), so `map` is fully runnable/typecheckable (`run --records`, `typecheck`) like `double` |
| `examples/double.v0.2.json` | example | Concrete v0.2 function record for `double` (a `nat -> nat` function); referenced by `map.v0.2.json`'s `examples.args[].fn_ref` |
| `examples/greet.v0.2.json` | example | Concrete **effectful** v0.2 function record: `greet : string -> unit`, declaring `effects: ["io.console"]`, body [`body-greet.json`](examples/body-greet.json). Demonstrates effect *enforcement* — `run` grants exactly its declared effects; the same body under `eval` is rejected without `--grant io.console` ([`evaluation.md`](evaluation.md)) |
| `examples/type-map.json` | example | The type of `map` (`forall a b. (a -> b) -> List a -> List b`) as a standalone structured type-expression AST |
| `examples/predicate-identity.json` | example | The identity property of `map` as a standalone structured predicate AST |
| `examples/value-list-int.json` | example | The list `[1, 2, 3]` of natural numbers as a standalone structured value AST |
| `examples/body-double.json` | example | The body of `double` as a structured body-expression AST: `\n -> add(n, n)` |
| `examples/body-is-zero.json` | example | A `case`-using body: `\n -> case n of 0 -> True; _ -> False` (exercises pattern matching and literal/wildcard patterns) |
| `examples/body-map.json` | example | The body of `map` as a structured body-expression AST: `\f xs -> map(f, xs)` (the commons `map` over the primitive `map`, as `double` is over the primitive `add`). `map.v0.2.json`'s `body_hash` resolves to it; `typecheck` confirms it against the declared `forall a b. (a -> b) -> List a -> List b` |
| `examples/body-greet.json` | example | The body of `greet`: `\msg -> print(msg)` — exercises the effectful `print` builtin (`io.console`). Runs only when the effect is granted (`run greet.v0.2.json` / `eval … --grant io.console`) |
| `examples/claim-satisfies-identity.json` | example | A `satisfies` claim asserting that the v0.2 map record satisfies its `identity` property |
| `examples/commitment-apply-double.json` | example | An `apply` commitment to call `double(42)` by end of 2026 |
| `examples/request.json` | example | Concrete `request` message (apply `map` to `[1,2,3]`); signed with deterministic seed `novae-linguae-example-claude` |
| `examples/request.v0.2.json` | example | Concrete **v0.2** `request` (the agent-loop input): apply `map` to (`double` as a `fn_ref`, `[1,2,3]`) with **value-expression** args; signed by `novae-linguae-example-claude` (`did:nova:ea9b…505e`), addressed to the example responder. Driven by `nl-validator respond` ([`agent-loop.md`](agent-loop.md)) |
| `examples/assert-result.v0.2.json` | example | The responder's signed `assert` reply to `request.v0.2.json`, produced by `nl-validator respond`: a `predicate` claim `eq( map(double, [1,2,3]), [2,4,6] )`, threaded by `in_reply_to` and addressed back to the requester. `nl-validator verify-claim … --records examples/` re-runs it to CONFIRMED |
| `examples/request-validate.v0.2.json` | example | A v0.2 `request` with `action: validate` for `double`; signed by the example claude identity. Driven by `nl-validator respond` ([`agent-loop.md`](agent-loop.md)) |
| `examples/assert-verified.v0.2.json` | example | The responder's signed `assert` reply to `request-validate.v0.2.json`: a `verified` claim (double verified **by** the responder's DID), after it typechecked the body and ran the examples. A `reject` is emitted instead when validation fails |
| `examples/query.v0.2.json` | example | A v0.2 `query` for records whose effects include `io.console`; signed by the example claude identity |
| `examples/ack-query.v0.2.json` | example | The responder's signed `ack` reply to `query.v0.2.json`: `result.matches` lists the sole match (`greet`), threaded by `in_reply_to`. Discovery over Nova Locutio (principle 4) |
| `examples/propose.v0.2.json` | example | A v0.2 `propose` to apply `double` to `[21]` (a proposal invites action but allows refusal); signed by the example claude identity |
| `examples/commit-apply.v0.2.json` | example | The responder's signed `commit` reply to `propose.v0.2.json`: an `apply` commitment to run `double(21)` (the responder test-ran it first), threaded by `in_reply_to`. A `reject` is emitted instead when it can't fulfil |
| `examples/delegation/delegate-root-to-alice.json` | example | A signed `delegate` token: the example **root** identity grants alice the broad `cap:apply`. Root of the 2-hop chain `nl-validator verify-delegation` checks |
| `examples/delegation/delegate-alice-to-bob.json` | example | A signed `delegate` token: alice **attenuates** her `cap:apply` to `cap:apply/double` for bob (with a `condition`). Verifying the chain authorizes bob for `cap:apply/double` but not the broader `cap:apply` |
| `examples/attestations/attest-root-vouches-alice.json` | example | A signed `attestation`: the **root** identity `vouches-for` alice — the seed edge of the trust graph `nl-validator evaluate-trust` walks |
| `examples/attestations/attest-alice-trusts-bob-rust.json` | example | A signed `attestation`: alice `trusts-claims-about` bob in domain `rust_ingestion`. With `trust-policy.json`, bob is TRUSTED for `rust_ingestion` but not for another domain |
| `examples/trust-policy.json` | example | A local trust policy (`trusted_roots`, `min_distinct_paths`, `satisfied_conditions`) the reference policy engine evaluates trust + capability authorization against |
| `examples/assert.json` | example | Concrete `assert` message (v0.1 — string claim) claiming an identity property; signed with deterministic seed `novae-linguae-example-verifier` |
| `examples/assert.v0.2.json` | example | Concrete `assert` message (v0.2 — structured `satisfies` claim against `map.json`); signed with deterministic seed `test-agent-v02` |
| `examples/commit.v0.2.json` | example | Concrete `commit` message (v0.2 — structured `apply` commitment to run `map` on `[1,2,3]` by end of 2026); signed with deterministic seed `test-agent-v02` |
| `examples/store-request.json` | example | Concrete `store` request whose `payload` is an inline function record; `payload_kind` drives cross-file `$ref` validation of the payload against `function-record.schema.json`. Signed with deterministic seed `novae-linguae-example-store` |
| `examples/encrypted-envelope.json` | example | A v0.2 encrypted envelope sealing a payload to the `request.json` signer; opens with seed `novae-linguae-example-claude`. Validates against `encrypted-envelope.schema.json`. |
| `examples/encrypted-envelope-mlkem768.json` | example | A v0.3 post-quantum hybrid (`kex x25519-mlkem768`) envelope to the same recipient, carrying a per-recipient `kem_ct`; opens with seed `novae-linguae-example-claude`. Validates against `encrypted-envelope.schema.json`. |
| `examples/did-document.json` | example | The DID document for the `request.json` signer identity (`did:nova:ea9b…505e`), publishing its ML-KEM-768 key-agreement key; built from seed `novae-linguae-example-claude`. Validates against `did-document.schema.json`. |
| `conformance/` | v0.1–v0.2 | Language-neutral cross-implementation conformance vectors: [`manifest.json`](conformance/manifest.json) (hashing, signing, signature verification, type well-formedness, schema validation, surface-syntax round-trips) plus golden JCS preimages under `conformance/canonical/`; and [`encryption.json`](conformance/encryption.json) (X25519, (X)ChaCha20-Poly1305, HKDF, Ed25519→X25519, and a deterministic envelope) for v0.2 payload encryption. See [`conformance/README.md`](conformance/README.md). |

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
- Conditional `store`-payload validation in `message.schema.json`: a `payload_kind` discriminator selects the artifact schema that `payload` must satisfy, applied by cross-file `$ref` (resolved by the reference validator against sibling files in `spec/`)

**Well-formedness vs structural validation.** JSON Schema can only check shape. Several semantic constraints are real but live outside the schema; they are enforced by the reference validator at [`tooling/validator/`](../tooling/validator/). Today this covers type-expression well-formedness (variable scoping, rank-1 forall, uniqueness within sums and records, ctor-kind compatibility in `apply`). Predicate- and value-expression well-formedness will follow once the verifier engine engages with them.

## What v0.1 deliberately defers

These are real specifications that will arrive in their own schemas. v0.1 stringifies them so we can start populating the commons without blocking on the full design.

1. **Type expression sub-language.** v0.1 `signature.type` in `function-record.schema.json` is still a string in surface syntax. **RESOLVED** at the schema layer in [`type-expression.schema.json`](type-expression.schema.json) (nine kinds: `var`, `ref`, `builtin`, `forall`, `fn`, `apply`, `tuple`, `record`, `sum`) with well-formedness checks in `nl-validator check-type`; **made mandatory by [`function-record.v0.2.schema.json`](function-record.v0.2.schema.json)**. Deferred to a later version of the type-expression schema itself: kind annotations, higher-rank polymorphism, type classes / traits, linear/affine types, existential types, GADTs, type-level lambdas.
2. **Refinement / predicate expression sub-language.** v0.1 `signature.refinements[].expr` in `function-record.schema.json` is still a string in surface syntax. **RESOLVED** at the schema layer in [`predicate-expression.schema.json`](predicate-expression.schema.json), and **made mandatory by [`function-record.v0.2.schema.json`](function-record.v0.2.schema.json)**. The reference validator now also **verifies** refinements against the body (`nl-validator check-refinement`): a `post` predicate names the output through the reserved variable `result`, and each is discharged as `∀ params. (∧ pre) ⟹ post[result := body]` via the SMT/induction backend. New records SHOULD target v0.2; v0.1 records remain valid against their pinned schema.
3. **Property expression sub-language.** v0.1 `properties[].expr` is still a string. **RESOLVED** — shares `predicate-expression.schema.json` with refinements; made mandatory by `function-record.v0.2.schema.json`.
4. **Value representation in examples.** v0.1 allows any JSON value for `args` and `result`, with a bare-string-for-function-references convention. **RESOLVED** in [`value-expression.schema.json`](value-expression.schema.json) (eleven kinds: `bool`, `int`, `nat`, `float`, `string`, `bytes`, `unit`, `list`, `tuple`, `record`, `variant`, `fn_ref`); made mandatory by `function-record.v0.2.schema.json`.
5. **Body representation.** v0.1 references the body by hash (`body_hash`) but does not specify the body's structure. **RESOLVED** end-to-end in [`body-expression.schema.json`](body-expression.schema.json): structured AST with seven expression kinds (`var`, `lit`, `app`, `let`, `lambda`, `case`, `field`) and four pattern kinds (`wildcard`, `bind`, `variant`, `lit`). Embedded types and values are accepted as opaque objects at this layer and must validate independently against `type-expression.schema.json` and `value-expression.schema.json`. The reference validator `nl-validator hash` auto-detects body expressions from the top-level `kind` field; `--kind body` is available as an override. Example function records (`double.v0.2.json`) now point at the real `expr_<…>` content-address of their body (`body-double.json`), and `verify` confirms the chain end-to-end. Deferred to a later body-expression schema version: optional type annotations on `let`, multi-binding `let`, multi-arm lambda equivalence sugar, do-notation, effect rows.
6. **Canonical serialization for hashing.** ~~v0.1 mentions canonical serialization but does not define it.~~ **RESOLVED in [`canonical-serialization.md`](canonical-serialization.md)**: JCS (RFC 8785) over UTF-8 JSON, BLAKE3-256 as the hash. The reference validator/hasher at [`tooling/validator/`](../tooling/validator/) implements the procedure end-to-end; example records now carry real, reproducible hashes that `nl-validator verify` passes.
7. **Controlled intent-tag vocabulary.** **RESOLVED** in [`intent-tag-vocabulary.md`](intent-tag-vocabulary.md): sixteen top-level categories and a set of property-modifier tags, with an extension policy. The schema continues to accept any tag matching the path pattern; the vocabulary is the convention for cross-agent agreement.
8. **Claim and commitment expression sub-languages.** **FULLY RESOLVED.** Structured ASTs defined in [`claim-expression.schema.json`](claim-expression.schema.json) (four kinds: `predicate`, `satisfies`, `verified`, `attestation`) and [`commitment-expression.schema.json`](commitment-expression.schema.json) (three kinds: `apply`, `provide`, `refrain`). **Made mandatory** in [`message.v0.2.schema.json`](message.v0.2.schema.json): `assert_body.claim` and `commit_body.commitment` are now required structured AST objects validated by cross-file `$ref`. The v0.1 message schema retains the string form unchanged; v0.2 messages must use the structured form. Worked examples: [`examples/assert.v0.2.json`](examples/assert.v0.2.json) (`satisfies` claim) and [`examples/commit.v0.2.json`](examples/commit.v0.2.json) (`apply` commitment). Both carry real hashes and Ed25519 signatures that `nl-validator verify` passes.
9. **Multicast addressing.** **RESOLVED** additively in `message.schema.json`: the `to` field now accepts a single DID, an array of DIDs (multicast), or null (broadcast). Existing single-DID messages remain valid.
10. **Multi-algorithm signatures.** **RESOLVED** additively in `message.schema.json`: the `signature` pattern broadened to `<algo>:<base64>` with `algo` matching lowercase kebab-case. v0.1 implementations MUST produce and verify `ed25519:<base64>` and MAY accept other algorithm tags. Existing ed25519 signatures still match.
11. **Absolute deadlines.** **RESOLVED** additively in `message.schema.json`: `constraints` gains an optional `deadline_at` field carrying an ISO 8601 wall-clock instant, alongside the existing relative `deadline_ms`. May be combined; receiver honors whichever expires first.
12. **Cross-schema validation.** **RESOLVED.** `request_body` gains an optional `payload_kind` discriminator; `allOf` `if/then` branches validate `payload` against the appropriate schema (function record v0.1/v0.2, body expression, or a type / predicate / value sub-language artifact) by **cross-file `$ref`** into the `https://novae-linguae.org/spec/...` namespace. The reference validator resolves those references against sibling files in `spec/` (`nl-validator validate`, backed by `validate_with_refs`); see [`examples/store-request.json`](examples/store-request.json) for a worked `store` message whose payload is validated cross-file. The change is **additive** — messages without a `payload_kind` (apply/validate requests and any message predating this check) keep the bare object constraint on `payload` and remain valid. Mandatory structured claim / commitment ASTs remain deferred to the next message-schema major bump (item 8); they are independent of this.

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

All example records (`map.json`, `request.json`, `assert.json`, `assert.v0.2.json`, `commit.v0.2.json`, and the v0.2 function records) carry real hashes and (for messages) real Ed25519 signatures, reproducible via `nl-validator hash` / `nl-validator sign --seed <s>`. They PASS `nl-validator verify`.

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

# Cross-file $ref resolution: the store payload is validated against
# function-record.schema.json (a sibling file) via the payload_kind discriminator
./tooling/validator/target/release/nl-validator validate spec/message.schema.json         spec/examples/store-request.json

# v0.2 message schema — structured claim/commitment ASTs
./tooling/validator/target/release/nl-validator validate spec/message.v0.2.schema.json spec/examples/assert.v0.2.json
./tooling/validator/target/release/nl-validator validate spec/message.v0.2.schema.json spec/examples/commit.v0.2.json

# End-to-end hash + signature verify (messages get both)
./tooling/validator/target/release/nl-validator verify spec/examples/map.json
./tooling/validator/target/release/nl-validator verify spec/examples/request.json
./tooling/validator/target/release/nl-validator verify spec/examples/assert.json
./tooling/validator/target/release/nl-validator verify spec/examples/assert.v0.2.json
./tooling/validator/target/release/nl-validator verify spec/examples/commit.v0.2.json

# Type-expression well-formedness (beyond JSON Schema)
./tooling/validator/target/release/nl-validator check-type spec/examples/type-map.json

# Execute a body, run a record's examples as tests, and type-check it (spec/evaluation.md)
./tooling/validator/target/release/nl-validator run        spec/examples/double.v0.2.json --records spec/examples/
./tooling/validator/target/release/nl-validator typecheck  spec/examples/double.v0.2.json --body spec/examples/body-double.json
./tooling/validator/target/release/nl-validator check-properties spec/examples/double.v0.2.json --body spec/examples/body-double.json
# Generative property testing: enumerate a small domain EXHAUSTIVELY, else sample for a
# counterexample (EXHAUSTIVE / HELD / REFUTED+shrunk / UNGENERATABLE).
./tooling/validator/target/release/nl-validator check-properties spec/examples/double.v0.2.json --body spec/examples/body-double.json --generate --cases 300
# SMT proof over the UNBOUNDED domain (the rung above bounded checks): translate each forall law to
# SMT-LIB 2, prove the negation unsat (PROVED) or find a counterexample (REFUTED); list laws fall back
# to structural INDUCTION (base + step), with auxiliary-LEMMA discovery when a single unfold + IH stalls:
# first a curated catalog (e.g. reverse∘reverse → PROVED via reverse_append), then THEORY EXPLORATION
# (conjecture lemmas by enumerating+testing terms over the goal's ops, prove the survivors) for laws the
# catalog lacks. The emitted .smt2 files are re-checkable certificates (the goal's + each proved lemma's).
# Needs an SMT solver (z3 by default); UNKNOWN only when neither catalog nor exploration closes it.
./tooling/validator/target/release/nl-validator prove spec/examples/double.v0.2.json --body spec/examples/body-double.json --smt-out /tmp/certs
# Prove two functions semantically EQUIVALENT (∀x. f(x)=g(x)); CLUSTER a record set into behavioral-
# equivalence classes (canonical representative each); COMPOSE a pipeline's composite metadata.
./tooling/validator/target/release/nl-validator equiv   --body-f <f.json> --body-g <g.json>
./tooling/validator/target/release/nl-validator cluster --records spec/examples/
./tooling/validator/target/release/nl-validator compose spec/examples/reverse.json spec/examples/length.json
# Static effect inference: prove the body's effects ⊆ its declared signature.effects, without running.
./tooling/validator/target/release/nl-validator check-effects spec/examples/greet.v0.2.json --body spec/examples/body-greet.json --records spec/examples/
# Verify declared metadata structurally (no solver): a `terminates: always`, and an `O(…)` complexity
# bound + structured `cost` (naive `reverse` is SOUND at O(n^2) TIME yet size-PRESERVING output).
./tooling/validator/target/release/nl-validator check-termination spec/examples/length.json --body spec/examples/body-length.json
./tooling/validator/target/release/nl-validator check-complexity  spec/examples/reverse.json --body spec/examples/body-reverse.json
# Certify a record end to end: every "verified by default" check in one pass, one certificate (--json).
./tooling/validator/target/release/nl-validator certify spec/examples/reverse.json --body spec/examples/body-reverse.json

# Effect enforcement (spec/evaluation.md): `run` grants exactly the record's declared effects;
# a standalone body needs --grant for any effect its builtins perform (io.console / random / time /
# panic / fs.read / fs.write / net.read / net.write / process.spawn / alloc — net/process off by default).
./tooling/validator/target/release/nl-validator run  spec/examples/greet.v0.2.json --records spec/examples/
./tooling/validator/target/release/nl-validator eval spec/examples/body-greet.json --arg <str.json> --grant io.console
# Real I/O is replayable: capture the trace, then replay it with no grant and no I/O (principle 5).
./tooling/validator/target/release/nl-validator eval <body> --arg <path.json> --grant fs.read --trace-out trace.json
./tooling/validator/target/release/nl-validator eval <body> --arg <path.json> --replay trace.json

# The Nova Locutio agent loop (spec/agent-loop.md): answer a request by running the target, then
# re-run the resulting assert's claim to confirm it; or orchestrate the whole conversation.
./tooling/validator/target/release/nl-validator respond      spec/examples/request.v0.2.json --records spec/examples/ --seed novae-linguae-example-responder
./tooling/validator/target/release/nl-validator verify-claim spec/examples/assert-result.v0.2.json --records spec/examples/
./tooling/validator/target/release/nl-validator orchestrate  --records spec/examples/ --intent arithmetic --arg <nat.json> --seed novae-linguae-example-claude
# Verified orchestration: discover → signature-filter + trust-rank candidates → re-prove the function's
# property → apply → re-verify. With --policy + --attestation, an untrusted function aborts the run.
./tooling/validator/target/release/nl-validator orchestrate  --verify --records spec/examples/ --intent arithmetic --arg <nat.json> --seed novae-linguae-example-claude --policy <policy.json> --attestation <vouch.json>

# Verify a delegation chain (spec/trust-model.md): can the grantee wield the capability via a chain
# of signed `delegate` tokens back to a recognized root? Checks signatures, attenuation, expiry.
./tooling/validator/target/release/nl-validator verify-delegation --capability cap:apply/double \
  --grantee <bob-did> --root <root-did> --delegations spec/examples/delegation

# Evaluate trust under a local policy (spec/trust-model.md): build the attestation graph from signed
# attestation/retract messages, spread trust from the policy's roots (with a diversity requirement),
# and decide whether a subject is trusted. `authorize` is the capability counterpart (chain + conditions).
./tooling/validator/target/release/nl-validator evaluate-trust --policy spec/examples/trust-policy.json \
  --attestations spec/examples/attestations --subject <bob-did> --domain rust_ingestion
./tooling/validator/target/release/nl-validator authorize --policy spec/examples/trust-policy.json \
  --capability cap:apply/double --grantee <bob-did> --delegations spec/examples/delegation
```

Cross-file `$ref`s resolve against sibling schema files: when a schema references another by its `https://novae-linguae.org/spec/<version>/<file>` identifier, `nl-validator validate` maps that to `<file>` in the schema's own directory. The version path segment is logical only — all schema files live flat in `spec/`. Any JSON Schema 2020-12 validator can also be used for structural checks; the reference is byte-equality of hash and JCS form across implementations.

## Contributing

Schema changes go through the same review as any other change. **Schema changes that affect record interpretation must bump the version**, even if the JSON Schema would still validate prior records — semantic compatibility is stricter than syntactic compatibility.
