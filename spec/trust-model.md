# Trust model (v0.1)

## Purpose

This document describes how trust, authorization, and reputation work in *Novae Linguae*. The deliberate stance, anchored by principles 6 and 7 in the project [`README.md`](../README.md): **no central authority anywhere in the protocol**. What replaces it is laid out here.

This is normative for v0.1. Implementations of the verifier, the commons, and *Nova Locutio* tooling MUST respect the structural constraints described below. Particular trust *policies* (whose attestations count for what) are NOT normative — those are local agent decisions, by design.

---

## What is NOT in the trust model

By design, none of the following exist at the protocol layer:

- **Central authority or root of trust.** No certificate authority, no foundation-approved verifier, no recognized "official" agent whose word is binding on others.
- **Role-Based Access Control (RBAC).** No global role registry, no admin tier, no privilege hierarchy.
- **Global reputation score.** No single answer to the question "is X trusted?"
- **Approval queue for commons additions.** Once an agent signs a record and pushes it, it is in the commons; there is no gatekeeper.
- **Identity-based exclusion list at the protocol level.** The protocol does not provide a "banned agents" mechanism.

Any of these can be implemented *above* the protocol by a community choosing to adopt them — but they are not in the protocol, and adopting them is a local decision, not a global one.

---

## What IS in the trust model

Five primitives, all already present or sketched in the existing schemas:

1. **Identity via DIDs.** Self-sovereign; agents control their own keys.
2. **Integrity via Ed25519 signatures.** Every *Nova Locutio* message is signed; tampering is detectable.
3. **Authorization via capability tokens.** Possession of a signed capability authorizes the holder; capabilities are delegable and attenuable.
4. **Attestation via signed assertions.** Agents make signed claims about other agents and about records. Attestations are public, queryable, time-bounded.
5. **Local trust policy.** Each agent decides locally which identities and attestations to weight.

The model is the *composition* of these primitives into the patterns described below.

---

## Local trust policy

A trust policy is an agent's local mapping from *situations* to *acceptance criteria*. Example policies:

- "Accept assertions about correctness from any agent if a proof certificate is attached."
- "Accept delegations from `did:nova:abc` only if the expiration is within 1 hour."
- "Mirror records into my local commons copy if they are signed by any of `{did:nova:x, did:nova:y}`."
- "Trust claims from agent X about Z-domain matters if at least three other agents I trust have made `vouches-for` attestations about X within the last 30 days."

**Trust policies are entirely local.** Two agents can hold incompatible views of the same third agent's trustworthiness without protocol contradiction. There is no API for "is X trusted?" — the answer depends on who is asking.

Policies SHOULD be expressible declaratively (so they can be reasoned about, audited, shared, and inherited) but the policy language is implementation-defined for v0.1.

---

## Capability tokens

Authorization is by **possession**, not by identity. To perform an action requiring capability `cap:X`, an agent presents a signed capability token authorizing it.

### Token contents

A capability token includes:

- The capability string (e.g., `cap:fs/read/home`).
- The grantee (a DID, or `null` for bearer-style).
- The granter (a DID), with their signature.
- An optional expiration.
- An optional set of conditions (e.g., "valid only for paths below `/home/projects`").
- A delegation chain back to a root grant the receiver recognizes.

### Receiver verification

A receiver MUST verify:

- The signature chain validates back to a root the receiver recognizes per its local trust policy.
- The capability covers the requested action.
- Any expiration and conditions are satisfied.
- The current presenter matches the grantee (or the token is bearer-style).

### Attenuation

When agent A delegates a capability to agent B, A MAY narrow the grant. If A holds `cap:fs/read/home`, A may grant B only `cap:fs/read/home/projects`. B cannot delegate broader than what A gave; the chain enforces this. The attenuation history is preserved and verifiable end-to-end.

This is standard object-capability-style authorization, drawing on the OCAP tradition (E, Joe-E), UCAN (the Fission design), SPKI, and macaroons. It is not invented here.

---

## Attestations

An attestation is a signed *assertion* (in the *Nova Locutio* speech-act sense) by one agent about another agent, a record, or a property.

Examples:

- `did:nova:verifier` asserts that function record `fn_3a9b...` satisfies its declared `identity` property, with proof certificate `proof_4d8e...`.
- `did:nova:alice` asserts that `did:nova:bob` produces reliable Rust-ingestion contributions.
- `did:nova:carol` retracts a prior assertion she made about `did:nova:dave` (sent as a `retract` speech act referring to the original attestation's hash).

### Properties of attestations

- **Public** — typically published to the commons or broadcast via *Nova Locutio*. Privacy of attestations would defeat their purpose.
- **Content-addressed** — every attestation has a `msg_<hash>` and is itself a *Nova Locutio* message validated against `message.schema.json`.
- **Queryable** — agents can `query` the commons for attestations matching a pattern.
- **Time-bounded** — attestations MAY include an expiration; absent that, they SHOULD decay in influence over time per the receiver's local policy.
- **Retractable** — the original signer can retract via the `retract` speech act. Retractions stop *ongoing* trust effects but do not undo past decisions that already depended on the retracted attestation.

### Trust derivation

Trust derives from attestation chains: agent A trusts agent B because (1) A trusts agent C per A's local policy, and (2) C has attested to B in a way A's policy weights positively. This is structurally similar to PGP's Web of Trust, but uses signed assertions about *properties* (correctness, reliability for a domain, identity binding) rather than key signatures alone.

---

## Negotiation patterns

The existing *Nova Locutio* speech acts compose into common trust-negotiation patterns. **None of these require new protocol primitives.**

### Capability acquisition

1. Agent A sends `request` for an action requiring `cap:X`, with no capability presented.
2. Agent B replies `reject` with code `not_authorized`.
3. Agent A sends `request` to agent C, who may have authority to delegate `cap:X`.
4. Agent C sends `delegate` granting `cap:X` (possibly attenuated) to A.
5. Agent A retries the original `request` to B with the delegated `cap:X` attached.
6. Agent B verifies the chain, executes, replies `ack`.

### Trust establishment

1. Agent A wants to evaluate whether to accept assertions from agent B.
2. A sends `query` to the commons for attestations about B from agents A already trusts.
3. A receives `assert` messages and any relevant `retract` messages.
4. A updates local trust policy based on the responses.
5. A is now (or is not) willing to weight B's claims in subsequent decisions.

### Resource exchange

1. Agent A sends `propose` — here is a deal: I will do X if you do Y.
2. Agent B sends `commit` — I bind myself to Y, conditional on you doing X first.
3. Agent A performs X and sends `assert` of completion (optionally with proof).
4. Agent B verifies, performs Y, and sends `assert` of completion.
5. Both sides `ack` for closure.

### Revocation

1. Agent A discovers that a previously-trusted attestation from agent B about agent C is wrong — B was deceived, or B's keys were compromised, or B has changed view.
2. Whoever controls the attestation (B in the simple case) sends `retract` referring to the original attestation's hash.
3. Recipients SHOULD propagate the retraction through their own attestation graphs.
4. Recipients SHOULD re-evaluate any trust decisions that depended on the retracted attestation.

Revocation stops ongoing effects; it does not undo past action. An agent who accepted a record because B vouched for it does not "un-accept" the record when B retracts. Future decisions simply stop relying on the retracted vouching.

---

## Reputation as emergent property

There is no explicit reputation score in the protocol. Reputation **emerges** from:

- The set of attestations about an agent.
- How those attesting agents are weighted in the receiver's local policy.
- How recent the attestations are.
- How the attestations compose with prior direct interactions.

This is genuinely peer-driven. Two agents can hold meaningfully different views of any third agent's reputation, and neither view is "more correct" at the protocol level. This is the point — it is what "no central authority" means in practice.

A consequence worth being explicit about: **reputation-driven filtering at the agent level is a soft form of restriction**, even though no central party imposes it. This is consistent with principle 7 (open communication; local-only filtering): the protocol guarantees no central censor, but it does not — and cannot — guarantee that every agent will receive every message with equal weight.

---

## Failure modes

**Sybil attacks.** An adversary creates many DIDs that vouch for each other. The protocol cannot prevent this. Local trust policy SHOULD require attestations reachable through *diverse* paths, not concentrated in a single cluster. The reference policy engine implements this as `min_disjoint_paths`: the number of internally **vertex-disjoint** root→subject paths (a max-flow over the trusted subgraph with unit vertex capacities), so corroboration that all funnels through one intermediary counts as a single path — strictly stronger than counting distinct final attesters.

**Sock-puppet attestations.** Same shape as Sybil — one operator running many DIDs to inflate attestation count. Same mitigation: the vertex-disjoint-path requirement above, plus **confidence-weighting** (`min_confidence`, with attestation `confidence` combined by noisy-OR) and **recency decay** (`half_life_days`, so a flood of stale vouches loses weight over time).

**Key compromise.** If an agent's private key is leaked, an attacker can produce signed messages indistinguishable from the legitimate agent's. Mitigations: signed key-rotation announcements (a specific kind of attestation), short-lived capabilities by default, and key-pinning at the application layer.

**Stale attestations.** Attestations from agents who have stopped operating or whose keys have rotated linger. Mitigations: explicit expirations, time-weighted scoring in local policy, periodic re-attestation requirements.

**Echo-chamber trust.** An agent's policy weights only attestations from agents whose attestations are in turn weighted — a closed trust circle that excludes contrary information. This is a real failure mode and it is *not solvable at the protocol level*. It is a property of the agent's policy, not the protocol.

**Operator legal compliance.** An agent runs somewhere. Whoever runs it operates under some jurisdiction's law. Local law can compel disclosure of keys, content, or attestations. The protocol cannot grant immunity from law. This is the most important non-protocol failure mode and is the reason confidentiality (principle 6 extension: payload encryption in v0.2+) matters.

---

## What this means for the existing spec

Most of the trust model is *already expressible* in v0.1 of the schemas. The relevant pieces:

| Spec element | Where | What it does in the trust model |
|---|---|---|
| DIDs in `from` and `to` | `message.schema.json` | Self-sovereign identity |
| Ed25519 `signature` on every message | `message.schema.json` | Integrity, provenance |
| `capabilities` in `constraints` | `message.schema.json` | Authorization tokens |
| `assert` speech act | `message.schema.json` | Attestations |
| `delegate` speech act | `message.schema.json` | Capability grants |
| `retract` speech act | `message.schema.json` | Revocation |
| `query` speech act | `message.schema.json` | Discovering attestations |

**No new schema is required to implement v0.1 of the trust model.** All three reference pieces are now built:

- A capability verifier (validates token chains, attenuation, expiration, conditions). **Built** — `nl_validator::verify_delegation_chain` (`tooling/validator/src/delegation.rs`), exposed as `nl-validator verify-delegation`. It checks every token's Ed25519 signature, walks the chain back to a recognized root, enforces attenuation (no link may widen the grant — capability covering is prefix-on-segments), skips expired tokens (against a supplied verification instant), honours bearer tokens (`to: null`), terminates on cyclic delegations, and collects every `condition` along the chain for the policy layer to enforce. It is wired **behind the capability gate**: a responder configured with a `TrustPolicy` (recognized roots + a token pool) fulfils a capability-gated `apply`/`propose` only when the sender can exhibit a valid chain — listing the capability string no longer suffices.
- An attestation-graph query layer over the commons. **Built** — `nl_validator::AttestationGraph` (`tooling/validator/src/attestation.rs`). An attestation is a signed `assert` whose claim is of kind `attestation` (`<attester> <verb> <subject>`, verb ∈ {`vouches-for`, `trusts-claims-about`, `distrusts`} — open question 1 resolved, see [`claim-expression.schema.json`](claim-expression.schema.json)). The graph verifies each attestation's signature, drops the targets of authentic `retract`s, prunes expired edges, and answers structural queries (about a subject, by an attester, positive/negative edges). It also ingests **signed certification records** ([`certification.schema.json`](certification.schema.json), from `certify --sign`) as `certifies` edges — an *objective, re-checkable* attestation (`<certifier> certifies <function>`) on a **separate axis** from `vouches-for` (a positive certification does not make its certifier trusted; it records only that the function passed every verified-by-default check under that certifier). A `certified: false` record is not an endorsement and adds no edge; certifications are retractable and signature-checked like any attestation.
- A reference policy engine (consumes local policy declarations and evaluates incoming attestations against them). **Built** — `nl_validator::Policy` (`tooling/validator/src/policy.rs`), exposed as `nl-validator evaluate-trust` and `nl-validator authorize`. A small JSON policy declares `trusted_roots`, `max_depth`, `min_distinct_paths` (distinct-attester diversity), `allow_distrust_override`, and `satisfied_conditions`, plus three richer Sybil gates (all opt-in): `min_confidence` (confidence-weighting — attestation `confidence` propagated from roots and combined by noisy-OR), `half_life_days` (recency decay — a vouch's weight decays `0.5 ^ (age / half_life)` from its `issued_at`), and `min_disjoint_paths` (the number of internally **vertex-disjoint** root→subject paths, a max-flow with unit vertex capacities — corroboration funnelled through one intermediary counts once). **Trust derivation** spreads trust transitively from the roots over the attestation graph and admits the queried subject only when every configured gate is cleared (distinct attesters ≥ `min_distinct_paths`, aggregate confidence ≥ `min_confidence`, vertex-disjoint paths ≥ `min_disjoint_paths`), scoped to a `domain` if given, and no trusted agent `distrusts` it. **Capability authorization** wraps the delegation verifier with the policy's roots and then *enforces* the chain's conditions — a condition the policy does not declare satisfiable is grounds for refusal (this is what finally enforces, rather than merely surfaces, delegation conditions). **Certification trust** (`Policy::certification_verdict`, `nl-validator certified`) answers "is this function certified by a certifier *I* trust?" — it intersects the function's certifiers with the agents the policy derives as trusted, so a certificate signed by an unrecognized key counts for nothing. This lets an agent rely on a trusted third party's certification (trust-delegation) instead of re-running every check itself, and the verified agent loop records it (`orchestrate --verify` surfaces a `commons_certified` signal alongside its own local re-certification).

---

## Open questions

Tracked for v0.2+; not blockers for v0.1.

1. **Standard "trust verbs."** ~~Should there be a controlled vocabulary of attestation predicates so two agents naming the same trust relation use the same predicate?~~ **RESOLVED (v0.2):** yes. The `attestation` claim kind in [`claim-expression.schema.json`](claim-expression.schema.json) fixes a closed verb vocabulary — `vouches-for` (general positive trust), `trusts-claims-about` (positive, scoped to a `domain`), `distrusts` (negative). `verified-by` is already covered by the separate `verified` claim kind (artifact verification). The reference policy engine reasons over exactly these verbs.
2. **Capability default expiration.** Should capabilities expire by default if no expiration is specified? Lean yes for safety; v0.2 to decide whether the default is enforced at the schema layer or the policy layer.
3. **Revocation propagation.** Push-style (broadcast retractions reach everyone), pull-style (consumers periodically re-query), or hybrid? Hybrid is most realistic. v0.2.
4. **Cross-policy interoperability.** Two agents with incompatible local policies still need a shared baseline to negotiate. A small interoperability spec ("here is the minimum information any policy must produce when refusing a request") is worth writing in v0.2.
5. **Quorum primitives.** Should the protocol provide first-class "trust X if N of these M agents vouch for X" support, or leave this entirely to local policy? Probably the latter (it is expressible in policy), but flagging.
6. **Selective disclosure / zero-knowledge attestations.** Verifiable Credentials (W3C) already provides selective disclosure. Whether *Nova Locutio* should integrate VC primitives or remain agnostic at the attestation layer is a v0.2 question.

---

## Reading list

For implementers and reviewers, the closest precedents:

- **UCAN** (Fission): JWT-style signed capability chains, attenuation, revocation. The closest practical analog to what is described above.
- **OCAP-Sec** (Mark Miller et al.): object-capability theory, the foundational tradition.
- **SPKI/SDSI**: earlier capability-and-attestation work from the IETF, much of which is preserved in modern designs.
- **W3C Verifiable Credentials Data Model**: how to express attestations as signed structured claims, with selective-disclosure support.
- **PGP Web of Trust**: the long history of peer-driven trust graphs, including the failure modes that informed this design.
- **The Cryptographic Doom Principle** (Moxie Marlinspike): if you do anything cryptographic with bytes you have not yet verified, you are about to lose. Applies directly to signature-verification ordering in the receiver.
