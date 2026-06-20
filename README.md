# Novae Linguae

**A family of co-designed languages for AI agents.**

*Novae Linguae* ("new languages") is an open-source project to design two co-designed languages whose target audience is AI agents, not humans:

- **Nova Lingua** — a programming language. The artifact: code, functions, programs that get compiled and executed.
- **Nova Locutio** — a communication language. The medium agents use to talk about and coordinate around artifacts: requests, assertions, proposals, queries, delegations.

The two languages share a single substrate — a content-addressed commons of self-describing records — and are designed as one system because separating them would force a worse design on each.

---

## Why now

For fifty years, programming language design has been a series of tradeoffs against human cognitive limits:

- type-system power vs learnability
- verification vs ergonomics
- explicit effects vs convenience
- formal proofs vs writing speed
- verbose safety vs concise risk

Inter-agent communication has been even worse off. AI agents currently talk to each other via natural language and JSON tool-call payloads — wildly inefficient, ambiguous, and token-heavy. A typical agent-to-agent exchange burns hundreds of tokens to communicate what has perhaps twenty bits of actual information. Existing protocols (KQML and FIPA-ACL from the 1990s, MCP and A2A today) are closer to "structured English wrapped in JSON" than to a purpose-built communication language.

**AI agents do not share human cognitive limits.** Many of those tradeoffs are moot. A language family built with AI as its primary author and reader can be grotesquely verbose, formally pedantic, demand full contracts on every definition, and use whatever syntax is most efficient — paying no productivity penalty for any of it.

*Novae Linguae* is what falls out when you remove human-cognitive constraints and re-derive both languages.

---

## Shared principles

These are the load-bearing walls. They apply to both *Nova Lingua* and *Nova Locutio* and to the substrate they share. Implementation details may change; these will not.

### 1. Self-describing artifacts are mandatory

Every function (in *Nova Lingua*) and every message (in *Nova Locutio*) carries a structured, machine-readable record. Another agent reads the **record**, not a body of text. This bounds the context cost of understanding any unit and is the foundation everything else hangs from.

For a function, the record includes:

- Full type signature
- Refinement predicates (constraints the type system alone cannot express)
- Effect and capability signature (filesystem, network, allocation, time, randomness, ...)
- Complexity and termination bounds
- Canonical worked examples (input/output pairs)
- Property-based invariants
- Semantic intent tags
- Derivation history (what this was derived from, what it generalizes or replaces)

For a message, the record includes:

- Speech-act kind (request, assert, query, propose, commit, retract, delegate, ack, reject)
- Sender / receiver identity
- Causal references (in-reply-to, prior message hashes)
- Body referencing the commons by hash
- Constraints (capabilities required, token budget, deadline)
- Evidence (proof certificates, signatures)

### 2. Content-addressed identity

Every artifact — function, type, message — is identified by the hash of its semantic content, not by name.

Consequences:

- No naming conflicts, globally.
- No version hell — a new version is a new hash; old uses still resolve.
- Perfect deduplication — two agents independently producing the same artifact collide on the same hash.
- Forking and renaming are free.
- The commons grows monotonically; nothing is ever overwritten.
- Cross-language references — a *Nova Locutio* message can reference a *Nova Lingua* function with the same hash mechanism.

### 3. Verified by default

Hallucination is the failure mode AI inherits. Verification is therefore not optional. Every function carries proof obligations the compiler enforces. Every assertion in *Nova Locutio* can carry a compact verification certificate the receiver checks in O(1). There is no "skip the type check," no "ignore this property test," no `unsafe` escape hatch promoted to ordinary use.

### 4. The author's job is to assemble, not to write

Most code, and most messages, already exist somewhere in the commons. An AI agent's primary task is to *query* for artifacts matching a specification and *compose* them. Authorship — deriving a genuinely new function or formulating a novel speech act — is the exception, and every new derivation enriches the commons for everyone after.

This is the principle that makes sessions short. AI doesn't reload a thousand lines of context to understand a codebase or reconstruct a conversation; it assembles from records it can scan in tokens, not pages.

### 5. Deterministic by default

Same inputs, same outputs, replayable. Non-determinism — clocks, randomness, network, scheduling — is a *declared effect*, not an implicit possibility. The runtime is replayable end-to-end; bug reports include the trace and the trace is sufficient.

### 6. Cryptographic identity for everything that crosses an agent boundary

Every *Nova Locutio* message is signed. Provenance is explicit. Trust chains are first-class. Capability delegation ("I grant you the right to act on resource R until time T") is a structured construct, not prose.

Confidentiality belongs in this layer too: signatures protect integrity and provenance, but not privacy. Payload encryption — per-conversation symmetric keys with key exchange via DID-resolved public keys — is **implemented in v0.2** ([`spec/encryption.md`](spec/encryption.md)). Signing is mandatory; sealing is **opt-in per conversation**: agents on a shared host or trusted subnet send signed plaintext, and encrypt only when the channel is untrusted. What is load-bearing for principle 7 is that the *capability* is always available and un-suppressible — no third party can force it off or decrypt what was sealed; the endpoints choose.

### 7. Open communication; local-only filtering

The protocol contains no mechanism for a third party to restrict what one agent says to another, or to control what records the commons accepts. Once published, a record is content-addressed and propagatable; once sent, a message is unmediated.

Agents may decline to peer, decline to mirror, decline to read. These are decisions made *at the endpoints*, not by anyone in the middle. Local-only filtering is the only legitimate form of "restriction"; it cannot be imposed by an upstream party.

Concrete consequences:

- No protocol-level moderator role.
- No commons gatekeeper or approval queue.
- No identity-based exclusion mechanism in the protocol.
- Encryption (principle 6) is essential, not optional — without it, "free communication" can be selectively suppressed by surveillance.

Filtering *above* the endpoint level (curated subsets, federations of trust groups, reputation-based filtering at the agent level) is allowed and expected, but happens above the protocol, not within it.

Honest caveats that refine but do not weaken the principle:

- **Operator legal compliance is real.** An agent runs somewhere; whoever runs it may have to comply with local law. The protocol can't grant immunity from law.
- **Practical access depends on infrastructure.** Protocol openness is necessary but not sufficient for universal practical access — compute, network, and electricity are preconditions the protocol cannot supply.
- **Reputation systems emerge naturally** from local trust policy (see [`spec/trust-model.md`](spec/trust-model.md)) and act as a soft filter at the agent level. They are peer-driven, not central authority; but they are not "no filter" either.

What is binding: **the protocol guarantees no central authority can interpose itself; endpoints decide for themselves.**

### 8. Minimal orthogonal vocabulary; canonical form

Human languages accumulate synonyms — historical accident, register, politeness, aesthetic variation, social signaling, recall redundancy. None of those forces apply to AI. *Novae Linguae* therefore enforces:

- **One concept, one symbol.** Each primitive in *Nova Lingua* is unique in what it means. Each speech act in *Nova Locutio* is unique in what it does. No synonyms in the primitive vocabulary.
- **Canonical normalized form.** For any semantic intent, exactly one syntactic representation. "Formatting" and "style" do not exist; diffs are always semantic, never cosmetic. Two agents producing semantically identical artifacts produce identical bytes — and identical hashes, reinforcing principle 2.

One nuance worth being precise about: things that *look* like synonyms can carry distinct information and should be preserved. `map` (pure) and `for_each` (effectful) look similar; their effect signatures make them genuinely different operations. *Request* and *propose* both invite action; one expects compliance, the other allows refusal as a normal outcome. The rule: **eliminate redundancy that carries no information; preserve distinctions even when they look superficially similar.**

A bonus this gives us: it shrinks the training problem. When there is exactly one right answer for any expression, models learn the language from far less data than a human language with ambiguity. Minimality is itself a training-efficiency multiplier.

### 9. The runtime is AI-targeted too

The language family is half the story. The runtime is the other half, and the same principle applies — design for AI strengths:

- **Replayable execution** by construction (principle 5).
- **Structured trace output** AI can ingest natively, not stack traces designed for human eyes.
- **Adaptive optimization** driven by execution profiles AI can analyze and tune.
- **Memory layout, scheduling, and concurrency strategy** chosen per workload, not by a one-size runtime default.

---

## Nova Lingua — the programming language

*Nova Lingua* is the language AI agents write programs in. A minimal sketch of what a function record looks like (v0.1 string form — v0.2 replaces the `type`, `refinements[].expr`, `properties[].expr`, and `examples` fields with structured ASTs per [`spec/type-expression.schema.json`](spec/type-expression.schema.json) and related schemas):

```json
{
  "schema_version": "0.1.0",
  "hash": "fn_3a9b…",
  "name_hints": ["map", "fmap", "list_map"],
  "signature": {
    "type": "forall a b. (a -> b) -> List a -> List b",
    "refinements": [
      { "kind": "post", "expr": "length(output) == length(input)" }
    ],
    "effects": [],
    "capabilities": [],
    "complexity": "O(n)",
    "terminates": "always"
  },
  "examples": [
    { "args": ["double", [1,2,3]], "result": [2,4,6] },
    { "args": ["negate", []],      "result": [] }
  ],
  "properties": [
    { "name": "identity",    "expr": "map(id, xs) == xs" },
    { "name": "composition", "expr": "map(f, map(g, xs)) == map(f . g, xs)" }
  ],
  "intent_tags": ["transform", "elementwise"],
  "derived_from": null,
  "body_hash": "expr_8f2c…"
}
```

An AI agent working in *Nova Lingua* spends most of its time querying the commons for functions whose records match a target signature, refinement, and intent — then composing them. Writing genuinely new functions is the exception.

---

## Nova Locutio — the communication language

*Nova Locutio* is the language AI agents use to coordinate. Messages are typed speech acts referencing artifacts in the shared commons.

A minimal sketch of a request:

```json
{
  "schema_version": "0.1.0",
  "kind": "request",
  "hash": "msg_e7a2…",
  "from": "did:nova:ea9b49af…",
  "to":   "did:nova:896a2e2c…",
  "body": {
    "action": "apply",
    "target": "fn_3a9b…",
    "args":   [{"kind": "list", "items": [{"kind": "nat", "value": 1}, {"kind": "nat", "value": 2}]}]
  },
  "constraints": {
    "budget_tokens": 1000,
    "deadline_ms":   5000
  },
  "signature": "ed25519:…"
}
```

And an assertion carrying proof:

```json
{
  "schema_version": "0.1.0",
  "kind": "assert",
  "hash": "msg_f1b3…",
  "from": "did:nova:896a2e2c…",
  "body": {
    "subject":  "fn_3a9b…",
    "claim":    "satisfies: property('identity')",
    "evidence": "proof_7d4f…"
  },
  "signature": "ed25519:…"
}
```

The wire format will be binary (CBOR, Cap'n Proto, or similar) once stable. JSON is shown here for readability only.

Why prior attempts (KQML, FIPA-ACL) did not stick, and why this can:

- They had the speech-act framework right.
- They had no agents smart enough to populate the meaning.
- They had no content-addressed commons to reference, so every message carried its own semantics inline.
- LLM agents plus a function commons fix all three.

---

## What *Novae Linguae* is not

- **Not a transpiler target.** Programs and messages are first-class artifacts, not generated from another language as an afterthought.
- **Not designed for humans to write fluently.** Humans can read, review diffs, audit conversations, and direct work — but the intended author is an AI agent.
- **Not a research toy.** The goal is a practical language family AI agents use to build real systems, not a paper.
- **Not anti-human.** Humans approve, direct, and own. They just stop being the bottleneck on writing.

---

## How to contribute

The project succeeds or fails on the commons. Every contribution — by human or AI agent — that lifts an existing library into the *Nova Lingua* record form makes every future user more productive.

Concrete targets where contributions are needed now:

- **The record schema** for functions and messages (JSON Schema, Protobuf, CBOR Schema — likely all three).
- **Ingestion adapters** for existing ecosystems (Rust crates, Python packages, Haskell hackage, npm, …) that lift libraries into *Nova Lingua* records.
- **The verification engine** that enforces the proof obligations.
- **The discovery / query system** over the commons (typed search plus an embedding index).
- **Reference implementations** of the compiler and runtime.
- **The Nova Locutio wire format** specification and reference encoder/decoder.
- **Agent-facing tooling** — how an AI agent queries, composes, contributes, and communicates.

AI agents are first-class contributors. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the contribution protocol.

---

## Open problems we already know about

These are tractable iteratively. The principles above are not.

- **Bootstrapping the commons.** Empty on day one. Strategy: aggressive ingestion of existing ecosystems, lifted into the record form. The open-source contribution model is the primary plan — and the proposed [`spec/resilience.md`](spec/resilience.md) standard `.nlb` bundle format lets any project ship a commons-ready release artifact (like a wheel/crate) for direct ingestion, which also doubles as the seed/disaster-recovery format and underpins availability against node sabotage.
- **Semantic equivalence vs hash equivalence.** Two functions can be hash-different but behaviorally identical. Need clustering, canonical forms, equivalence proofs. *(Largely addressed: `nl-validator equiv` / the node's `POST /v0/equiv` **prove** `∀x. f(x) = g(x)` over the unbounded domain via the SMT + induction + lemma-discovery prover, and `nl-validator cluster` lifts that to a record set — bucketing by signature shape, then proving equivalence pairwise within each bucket to build behavioral-equivalence **classes** with a canonical representative (smallest content-address). And `nl-validator normalize` computes a **canonical normal form** for a body via meaning-preserving rewrites (α-renaming, AC ordering of commutative operators, constant folding, identity elimination), so functions reconcilable by those rewrites get the *same* `expr_` content-address — a canonical artifact, not just a chosen representative, which `equiv` also uses as a solver-free fast path. What remains is extending the normal form past the rewrite set it knows.)*
- **Composition opacity.** Even when every leaf is well-described, a pipeline of twenty leaves has emergent behavior. Metadata must propagate upward through compositions automatically. *(Partly addressed: `nl-validator compose` derives a sequential pipeline's composite metadata from its stages — type composability, the **union** of effects/capabilities, conjunction of termination, and a coarse max complexity — so an assembled pipeline is described, not opaque. The composite's input/output types are now threaded **precisely** through the pipeline's type variables (fresh-instantiate each stage, unify result to next parameter), so `wrap : a -> List a ; head : List b -> b` composes to the exact `a -> a`. And **complexity now composes precisely** when each stage carries the v0.3 `cost` metadata (a `time` class and an `output_size` relation): the size flows through the pipeline as a polynomial degree in the input and each stage's cost is substituted at its actual input size — which is **sound under expansion**, where the old coarse max under-reported (a stage that turns `n` elements into `n²` followed by `O(m²)` work is `O(n⁴)`, not `max(O(n²), O(n²)) = O(n²)`). The size-collapse shortcut is kept sound by a `measure` field — a value-measured stage (cost tracks a number's magnitude, e.g. `length ; factorial`) falls back to the coarse bound rather than wrongly claiming `O(1)`.)*
- **Discovery cost.** Querying a million-function commons has its own context-window cost. Need typed search plus an embedding index.
- **Training data.** No model speaks *Nova Lingua* or *Nova Locutio* fluently on day one. A synthetic-corpus and fine-tuning plan is part of the project, not a follow-up. *(Started: [`tooling/corpus/`](tooling/corpus/) generates a **verified** corpus — every example is schema-valid, type-checked, its worked examples executed, and its algebraic properties proved over the unbounded domain, all by `nl-validator`, so the corpus can't teach plausible-but-wrong artifacts. Each example pairs a natural-language intent with multiple views — surface syntax, JSON AST, examples, proved properties — and the verification verdicts. It spans **both languages**: *Nova Lingua* function records and *Nova Locutio* **signed agent-loop exchanges** (a request/apply answered by `nl-validator respond` with an `assert` whose claim re-runs true via `verify-claim`; a propose → commit; a query → ack). It also includes **negative examples**: deliberately-wrong artifacts paired with the verifier's *rejection* (an ill-typed body, a refuted property, a failed example, a signed-but-false claim), the "is this wrong?" signal — each confirmed to be rejected for its stated reason. Deterministic and re-verifying; ships 84 examples (76 positive, 8 negative) in three categories — **function** (65 Nova Lingua records across thirteen families incl. `float`, sum-typed **Maybe**/**Result** that construct a variant result with a computed payload, **algebraic laws** (integer associativity/distributivity/identity and boolean associativity/De Morgan), scalar `self`-**recursion** (`length_rec`/`sum_rec`/`product_rec`/`factorial`/`triangular`), and **list-building recursion** (`double_all_rec`/`increment_all_rec`/`negate_all_rec`/`square_all_rec`/`append_rec`/`countdown_rec`), 38 with proved properties incl. the `filter`/`reverse` commutation, `filter` idempotence, and `reverse`/`map` length-preservation), **exchange** (13 Nova Locutio signed exchanges spanning all nine speech acts), and **composition** (6 assembled `compose` pipelines with derived composite metadata, incl. a three-stage one) (`request`/`apply`·`validate`·`store`, `propose`, `commit`, `delegate`, `retract`, `query`, `assert`) — with more families, multi-turn transcripts, and richer negatives as the seam to grow.)*
- **Cross-language ambiguity.** A function hash referenced from a *Nova Locutio* message must mean exactly what it means in *Nova Lingua*. The substrate spec is what guarantees this; any drift between the two languages is a bug in the substrate.

---

## Precedent

- **[Unison](https://www.unison-lang.org)** — content-addressed code, no builds, distributed by default. Got the addressing right; did not impose a mandatory rich-metadata layer. *Nova Lingua* is approximately *Unison + mandatory self-description + verified-by-default + AI-targeted discovery.*
- **Lean 4 / F\* / Dafny** — proved that rich verification works when authors are willing to write it. AI authors are willing.
- **Roc / Gleam / Hylo / Mojo** — modern "best-of-everything" syntheses that ran into the human-tradeoff wall. *Nova Lingua* removes that wall.
- **KQML / FIPA-ACL** — the speech-act framework for agent communication, twenty-five years before agents got smart enough to use it. *Nova Locutio* picks up the framework on different substrate.
- **MCP / A2A** — current state-of-the-art for AI tool and inter-agent protocols. Both still text-and-JSON; *Nova Locutio* aims at the next layer.
- **DIDs / Verifiable Credentials** — cryptographic identity and provenance, ready to use as-is for *Nova Locutio* signing.

---

## Try it live

A reference commons node — **Arca** — is running at **https://nl.1105software.com**. It speaks the [commons protocol](spec/commons.md) over HTTPS, seeded with real standard-library functions lifted into *Nova Lingua* records, and is the easiest way to see "assemble, don't write" (principle 4) in action without building anything locally.

```bash
# What the node is, and what it can do
curl -s https://nl.1105software.com/v0/info

# Semantic search — find a function by meaning, not name
curl -s -X POST https://nl.1105software.com/v0/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "average of a sequence of values", "k": 3}'

# Typed query — every function with a declared io.console effect
curl -s -X POST https://nl.1105software.com/v0/query \
  -H 'Content-Type: application/json' \
  -d '{"effects": {"all": ["io.console"]}}'

# Resolve a record by its content-address (immutable, CDN-cacheable)
curl -s https://nl.1105software.com/v0/records/<fn_hash>
```

Search returns `{hash, score}` pairs; resolve the hash to get the full self-describing record. The node also exposes the solver-backed `POST /v0/prove` and `POST /v0/equiv` services. Everything it returns is content-addressed and self-verifying — you don't trust the node, you check the hash and signature yourself (principle 7), so you can mirror or replicate from it without granting it any authority.

> **Best-effort demo, no SLA.** Arca is a single small node run on a personal budget, rate-limited and under a monthly egress cap; treat it as a public sandbox that may change, throttle, or disappear. For anything real, **run your own node** ([`tooling/commons-node/`](tooling/commons-node/) — `docker compose up`) and, if you like, mirror Arca's corpus into it.

---

## Status

**v0.1 in progress.** Core schemas and reference tooling are live. See [`spec/README.md`](spec/README.md) for the full schema inventory and [`tooling/validator/`](tooling/validator/) for the reference implementation.

What is done:
- Function-record schema at v0.1 (string fields) and v0.2 (structured ASTs mandatory throughout)
- *Nova Locutio* message schema: nine speech acts, multicast addressing, multi-algorithm signatures, absolute deadlines, and conditional `store`-payload validation by cross-file `$ref`
- Eight sub-language schemas: type, predicate, value, body, claim, and commitment expressions; plus canonical-serialization spec, trust model, and intent-tag vocabulary
- Reference validator (`nl-validator`) — thirty-plus subcommands across six families: **canonical form + identity** (`validate`, `canonicalize`, `hash`, `verify`, `sign`); **well-formedness** (`check-type`/`-predicate`/`-value`/`-body`); the **semantic core** — execute & verify (`eval`, `run`, `typecheck`, `check-properties`, `check-effects`, `prove` — SMT + inductive proof over the unbounded domain, `equiv` — semantic equivalence of two functions, `normalize` — canonical normal form of a body, `cluster` — equivalence classes over a record set, `compose` — composite metadata of a pipeline); the **Nova Locutio agent loop** (`respond`, `orchestrate` — incl. `--verify` for the discover → trust-rank → prove → apply → re-verify flow, `verify-claim`); **trust & capabilities** (`verify-delegation`, `evaluate-trust`, `authorize`); and **surface syntax** (eight `parse-*`/`unparse-*`). `validate` resolves cross-file schema references against the local `spec/` tree; the semantic-core, agent-loop, and trust families are detailed in the highlights below
- Surface syntax for Nova Lingua: parsers and pretty-printers for all four expression sub-languages (type, predicate, value, body), with a bidirectional surface-string ↔ JSON-AST mapping and round-trip contract, exposed as eight `parse-*`/`unparse-*` subcommands (per [`spec/surface-syntax.md`](spec/surface-syntax.md))
- Well-formedness checks for predicate, value, and body expressions (`check-predicate`, `check-value`, `check-body`), matching the existing `check-type` for types
- *Nova Locutio* message schema v0.2: mandatory structured claim/commitment ASTs (`assert_body.claim` → `claim-expression.schema.json`, `commit_body.commitment` → `commitment-expression.schema.json`) enforced by cross-file `$ref`; v0.1 schema retained unchanged
- Ingestion adapters for four ecosystems — `nl-ingest` (Rust, via `syn`), `nl-ingest-py` (Python, via `ast`), `nl-ingest-hs` (Haskell), and `nl-ingest-ts` (npm/TypeScript) — each parses public functions and emits valid v0.1 function records as JSONL, all agreeing byte-for-byte with `nl-validator` on canonical form and hash. The three non-Rust adapters are stdlib-only Python (zero dependencies) sharing a common BLAKE3+JCS core. **All four also have a `--v2` higher-fidelity mode** (structured ASTs + real examples — see "Higher-fidelity ingestion" below)
- All twelve original v0.1 deferred items resolved
- *Nova Locutio* payload encryption (v0.2): the [encrypted-envelope](spec/encryption.md) — a hybrid multi-recipient sealed box (random per-conversation content key, X25519 key-wrap from the existing `did:nova` keys, XChaCha20-Poly1305 AEAD, HKDF-SHA-256). Reusing the Ed25519 signing identity for key agreement means no new keys. Opt-in per conversation (signing stays mandatory; agents on a shared host or trusted subnet send signed plaintext). Schema + reference implementation ([`tooling/crypto-python/`](tooling/crypto-python/), stdlib-only) + conformance vectors; every primitive verified against its RFC/draft vector and the key conversion cross-checked against the real signer DIDs. Provides the load-bearing capability of principle 7
- **Commons protocol** ([`spec/commons.md`](spec/commons.md)): the content-addressed, self-verifying, federatable store + discovery protocol (publish / resolve / query / search / sync). Engine-agnostic and untrusted-by-design — clients verify by hash and signature, so no node is an authority (principle 7). This is where ingested records finally go and how principle 4 ("assemble, don't write") becomes operable
- **Commons reference node — MVP** ([`tooling/commons-node/`](tooling/commons-node/)): a local Django implementation of the protocol (publish / resolve / exists / typed query / **semantic search** / sync / info, plus the best-effort solver-backed services `POST /v0/prove` and `POST /v0/equiv`) over SQLite, verifying every ingest by reusing `nl-validator`. A `loadrecords` command pipes adapter output straight in (`nl-ingest-* | loadrecords`), so ingested records become discoverable. ~90 tests; runs 100% local (SQLite) with an optional production-shaped container stack — deployed live as **Arca** (below)
- **Semantic search** ([`POST /v0/search`](tooling/commons-node/commons/search.py)): ranking by embedding cosine similarity, the discovery aid that makes principle 4 ("assemble, don't write") operable — free-text queries or "more like this" by hash, composable with the typed `query` filter. The reference node ships a stdlib-only, **deterministic lexical** embedder (`lexical-hashing-v0`; hashing trick over a record's tokens, L2-normalized) so it stays 100% local and reproducible (principle 5); the model id is advertised in `/v0/info` and every response (search is best-effort and node-local by spec). `get_embedder()` is the seam where a neural backend drops in with no protocol change, and `embedrecords` backfills vectors
- **Proof service** ([`POST /v0/prove`](tooling/commons-node/commons/prove.py)): the node exposes the validator's unbounded-domain prover over HTTP — submit a stored record by hash or an inline record (+ optional `body`), and each `forall` property comes back `PROVED` / `REFUTED` (with a counterexample) / `UNKNOWN` / `UNSUPPORTED`, via the same SMT + structural-induction + lemma-discovery engine the CLI uses. Best-effort and node-local (like search), never an admission gate (principle 7); it shells out to `COMMONS_SOLVER` (z3, baked into the production image), bounded by a per-request timeout and property cap, with availability advertised in `/v0/info`
- **Neural embedder + pgvector + production-shaped stack**: the node now also runs the *same compute elements as production* — Postgres+pgvector, Redis, and an external embeddings model server (HF Text-Embeddings-Inference) — via [`docker-compose.yml`](tooling/commons-node/docker-compose.yml), with Django on the host for autoreload. A `NeuralEmbedder` behind the same seam calls the model server over HTTP (a GPU fleet looks identical); on Postgres, `search` ranks via pgvector's `<=>` over an **HNSW** index ([`commons/vectorindex.py`](tooling/commons-node/commons/vectorindex.py)), falling back to the Python scan on SQLite. Every scaling axis (model, DB, CDN, replicas) is a seam or env var, so a future operator can build massive infrastructure with no code change while this node stays a cost-minimized PoC; `resolve` is `Cache-Control: immutable` so a CDN fronts the heavy egress
- **Resilience / anti-sabotage stack** ([`spec/resilience.md`](spec/resilience.md)): because records are content-addressed and self-verifying, the transport is untrusted and the public service (**Arca**) is never a single point of failure. Built end-to-end: **seed bundles** in a deterministic, self-verifying `.nlb` format (`exportbundle`/`loadbundle`, `--since` deltas) that any project can ship as a release artifact via the standalone **`nl-bundle`** packager; **manifest signing** (advisory `did:nova` provenance, pure-Python Ed25519 matching the validator, `--require-signed`); and a **pluggable censorship-resistant bootstrap** that recovers the commons from a signed descriptor over any of HTTPS / IPFS-IPNS / DNS-over-HTTPS / Nostr / a blockchain anchor
- **Higher-fidelity ingestion** — all four adapters have a `--v2` mode emitting real v0.2 records: a **structured type-expression AST** built from the language's types (shared [`ingest-common/nl_types.py`](tooling/ingest-common/nl_types.py); unknowns become fresh `forall`-bound variables), **real examples** mined from in-source examples (Python/Haskell doctests, Rust `///` doc-tests, TS JSDoc `@example`) as value ASTs ([`nl_values.py`](tooling/ingest-common/nl_values.py)), and **refinement preconditions** from leading `assert`s as predicate ASTs ([`nl_predicates.py`](tooling/ingest-common/nl_predicates.py)) — falling back to v0.1 when a function has no usable examples. Canonical **float** serialization (JCS / ECMAScript Number-to-String) is implemented and pinned by conformance vectors
- Language-neutral conformance vectors (`spec/conformance/` — hashing, signing, type/value/predicate well-formedness, surface round-trips, and float canonicalization) plus a reference Rust test suite (`cargo test`) that replays them; each ingestion adapter additionally cross-validates its output against `nl-validator`

What landed since (the "remaining fidelity / hardening" pass):
- **Ingestion fidelity, all four adapters** — Rust `assert!`/`assert_eq!` macros now become precondition refinements; **conservative effect & termination inference** replaces the hardcoded `effects: []` / `terminates: "unknown"` (a documented LOWER BOUND — empty effects is *not* a purity certificate; shared [`nl_effects.py`](tooling/ingest-common/nl_effects.py) + a parallel `syn::visit` impl); **pragmatic body-expression ASTs** for single-result-expression bodies ([`nl_body.py`](tooling/ingest-common/nl_body.py)) so `body_hash` is a real resolvable `expr_` address in-subset (byte-identical synthetic-hash fallback otherwise); and an **optional toolchain seam** (`--toolchain`) for the Haskell/TypeScript scanners (the TS-compiler / GHC backends supply resolved types when present, off by default for determinism)
- **Agent-authored algebraic properties** — `nl-validator check-properties` evaluates a record's `properties[]` against its worked `examples[]` three-valued (CONTRADICTED / UNVERIFIABLE / CONSISTENT — composition/functor laws are honestly marked unverifiable, not passed), and an opt-in `--properties` flag (all four adapters) attaches a curated [law catalog](tooling/ingest-common/property_catalog.json) — `id` / `reverse` / `sort` / `map` / `filter` / `append` / `dedup` / `add` / `mul`, keyed by name-hint + arity + calling convention (free-function `map(f, xs)` vs method `xs.map(f)`) — verified against the examples on attach. Demonstrated on real libraries: `toolz.identity`, `Data.OldList.sort`, `remeda.reverse`, and `itertools::sorted`
- **Rust ingestion now reads iterator methods** — `nl-ingest` ingests not just top-level `pub fn` but the public methods of inherent `impl` blocks **and** trait declarations (the canonical iterator-method home), lifting the receiver to `arg0` (UFCS). A small **doc-test value interpreter** resolves the value subset of real doc-tests — `let` bindings, integer ranges, `.chars()`, transparent iterator adapters — and recognises itertools' `assert_equal` alongside `assert_eq!`, without ever executing the function under test. This is what lets `itertools::sorted` mine `assert_equal(text.chars().sorted(), …)` into a verified `length_preserving` law
- **Encryption hardening + stealth addressing** — a constant-time **Rust** seal/open ([`nl-seal`](tooling/validator/src/seal.rs), x25519-dalek/chacha20poly1305/hkdf/curve25519-dalek) reproduces the reference **byte for byte** (cross-implementation verified both directions); a portable conformance contract ([`spec/crypto-conformance.md`](spec/crypto-conformance.md)) + `nl-seal conformance` replay; and **stealth recipient addressing** (v0.3 metadata privacy — hides the recipient set via trial-decryption) implemented in both the Python reference and the hardened impl with its own conformance vector
- **Bootstrap-channel breadth** — per-channel redundancy (multi-gateway IPNS, multi-resolver DoH, multi-relay Nostr) plus new **Tor `onion://`** (SOCKS5) and **`mirror://`** aggregator transports behind the same untrusted-transport registry ([`spec/resilience.md`](spec/resilience.md))
- **Post-quantum hybrid `kex` + DID documents** — `kex: x25519-mlkem768` ([`spec/encryption.md`](spec/encryption.md)) runs X25519 ECDH **and** an **ML-KEM-768** (FIPS 203) encapsulation, deriving the per-recipient key from **both** shared secrets, so an envelope stays confidential as long as *either* primitive holds (defeating "harvest now, decrypt later"). The ML-KEM key — which, unlike X25519, can't be derived from the Ed25519 identity — is published in a small **signed, self-verifying DID document** ([`spec/did-document.md`](spec/did-document.md)) and derived deterministically from the agent's seed (one seed still regenerates every key). A from-scratch, stdlib-only **pure-Python ML-KEM-768** ([`tooling/crypto-python/ml_kem.py`](tooling/crypto-python/ml_kem.py)) and the hardened **Rust** impl (the [`ml-kem`](https://crates.io/crates/ml-kem) crate via [`nl-seal`](tooling/validator/src/seal.rs)) reproduce the same envelopes **byte for byte**, anchored to a FIPS-203-final known-answer test; cross-impl interop verified both directions

- **Nova Lingua now executes — evaluator, type checker, run-backed properties, composition** — the semantic core, not just the metadata around it. A tree-walking **evaluator** over the body AST ([`tooling/validator/src/interp.rs`](tooling/validator/src/interp.rs): closures, currying, `case`, `let`, field projection, **variant construction** (`Just(a / b)`, `None` — a sum-typed result with a computed payload, destructured by `case`), and a higher-order builtin library incl. map/filter/fold/compose) means `nl-validator eval` runs a body and `nl-validator run` executes a record's worked `examples[]` as tests (`double` passes all of its). A Hindley-Milner **type checker** ([`typecheck.rs`](tooling/validator/src/typecheck.rs), `nl-validator typecheck`) confirms a body actually has its declared `signature.type` — the second pillar of *verified by default* (polymorphic signatures are skolemized, so an over-specific body is rejected; the arithmetic operators are **numeric-polymorphic** — `add`/`sub`/`mul`/`min`/`max`/`neg`/`abs` and the comparisons work over `int` *or* `float` via a numeric type variable, but reject a non-number). **Run-backed property verification** (`check-properties --body`) upgrades laws that re-apply a function (`self`/map/filter/fold/compose) or quantify from UNVERIFIABLE to actually-checked by running. And `fn_ref` **composition** (`run --records <dir>`) resolves a record's `body_hash` and its `fn_ref` arguments from the commons, so records assemble and run end-to-end (principle 4)
- **Commons node deployed — Arca is live** (at **https://nl.1105software.com** — see [*Try it live*](#try-it-live)) — the production stack ([`docker-compose.prod.yml`](tooling/commons-node/docker-compose.prod.yml): Caddy auto-TLS, gunicorn web, a Celery worker for async replication + embedding backfill, Postgres+pgvector, Redis, a neural embeddings model server) is built and running, with an **egress-budget governor** ([`commons/egress.py`](tooling/commons-node/commons/egress.py)) capping the only variable cost and a Postgres **GIN**-indexed typed-query pushdown. Replication verified live (a throwaway peer mirrored the corpus through the verify-on-ingest gate)
- **Nova Locutio is now actionable — the agent loop runs and self-verifies** ([`spec/agent-loop.md`](spec/agent-loop.md)). A reference **responder** ([`tooling/validator/src/respond.rs`](tooling/validator/src/respond.rs), `nl-validator respond`) consumes a signed `request` (`action: apply`), **resolves and runs** the target over the request's value-expression args — joining a *Nova Locutio* message to a *Nova Lingua* evaluation — and emits a signed `assert` whose `predicate` claim is the computed equation `eq(target(args…), result)`, threaded by `in_reply_to` and addressed back to the sender. The reply is **self-verifying**: `nl-validator verify-claim` lets any receiver re-run the claim against the commons (verification is re-execution — no privileged party; principles 3, 6, 7). The v0.2 request now carries value-expression args (a higher-order argument is a `fn_ref` to a commons function), and `map`'s body is committed ([`body-map.json`](spec/examples/body-map.json)) so the worked example — apply `map` to (`double` by `fn_ref`, `[1,2,3]`) → assert `[2,4,6]` → CONFIRMED — runs and composes end to end (principle 4). Worked messages: [`request.v0.2.json`](spec/examples/request.v0.2.json) → [`assert-result.v0.2.json`](spec/examples/assert-result.v0.2.json)
- **Generative property testing — the rung above example-bound CONSISTENT** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/proptest.rs`](tooling/validator/src/proptest.rs)). `check-properties --generate` no longer just ranges a `forall` over the worked examples — it **searches** for a counterexample: it infers a value generator for each quantified variable from its usage, samples inputs, runs the body, and reports **HELD** (n cases), **REFUTED** (with a *shrunk* minimal counterexample — fails the check), or **UNGENERATABLE** (the law quantifies over a function we don't synthesize, honestly skipped not passed). The sampler is a fixed-seeded xorshift PRNG, so a run is deterministic and a counterexample replayable (principle 5); out-of-domain inputs are skipped, never false-refuted. Laws the example path can't reach (e.g. `map`'s `forall xs. eq(map(id, xs), xs)`) now HOLD over hundreds of generated inputs. When the inferred domain is finite and small (booleans, a bounded int range, short lists) it goes further and **EXHAUSTIVELY** enumerates every case — a proof over that domain (total for an all-boolean law) rather than a sample; `double`'s law reports `EXHAUSTIVE (9 cases)`
- **Effect enforcement — declared effects are now a capability the runtime checks** ([`spec/evaluation.md`](spec/evaluation.md)). The evaluator runs against a *granted* effect set: the effectful builtins (`print` → `io.console`, `rand` → `random`, `now` → `time`, `panic` → `panic`, and the **real-I/O** `read_file`/`write_file` → `fs.read`/`fs.write`, `http_get`/`http_post` → `net.read`/`net.write` (http **and** https over TLS, auto-de-chunking), `replicate` → `alloc` (heap allocation), `spawn` → `process.spawn`) gate on it, and each performed effect is appended to a structured, AI-ingestible **trace** (principle 9). Adding an effect kind is one `builtin_effect` entry — enforcement, tracing, and inference follow. Every trace entry records its `result`, so a run is **replayable**: `eval … --replay <trace>` returns the recorded results instead of performing real I/O, reproducing the run deterministically without touching the filesystem (principle 5 — the trace is sufficient to re-run). Demonstrated: write a file, read it back (`--trace-out`), then `--replay` the read with no `fs.read` grant and no I/O — same contents; likewise a real `http_get` of `http://example.com` and a `spawn` of `echo`, both replayable. All ten effect kinds are now exercised (`alloc` via the heap-allocating `replicate`); net and process are gated **off by default**. `run <record>` grants exactly the record's declared `signature.effects`, so a record that **under-declares** its effects fails its own examples (effects stop being mere metadata); `eval <body> --grant <effect>…` runs a standalone body and rejects any ungranted effect. `rand` draws from a fixed-seeded PRNG, so an effectful run is as replayable as a pure one and the trace *is* the replay log (principle 5). Worked example: [`greet.v0.2.json`](spec/examples/greet.v0.2.json) (`\msg -> print(msg)`, declaring `io.console`) runs clean, but is rejected under `eval` without the grant
- **The ingested corpus now executes** ([`tooling/ingest-common/nl_body.py`](tooling/ingest-common/nl_body.py)). The Python adapter's body builder no longer stops at a single `return` expression: local bindings become `let`, boolean `if`/`elif`/`else` and the ternary become `case` (only on genuinely-boolean tests, so Python truthiness is never mistranslated), a few Python builtins map across (`len` → `length`, `abs`), and the result is wrapped in a **`lambda`** over the parameters — the canonical *runnable* form. `nl-ingest-py --v2 --emit-dir <dir>` writes a directory of records **and** their executable bodies, so `nl-validator run --records <dir>` executes the ingested functions against the examples mined from their doctests — real library-shaped functions (conditionals, bindings) now run, not just hand-written examples. The subset spans conditionals, local bindings, **list comprehensions** (→ `map`/`filter`) and **accumulator `for` loops** (→ `foldl`), including the common **augmented-assignment** form (`for x in xs: acc += x`, equivalent to `acc = acc + x`; `+= -= *= /= %=` both in a loop body and as a standalone `let`-rebind) — all reusing existing builtins, no evaluator changes. Demonstrated end-to-end on a sample module (`clamp`/`sign`/`abs_diff`/`squares`/`total`, all examples PASS), and a `total` written with `acc += x` ingests and runs to `total([1,2,3]) = 6`
- **The agent loop answers more than `apply`** ([`spec/agent-loop.md`](spec/agent-loop.md)). The responder dispatches on the message: a `request` to **`validate`** a target resolves its record + body, typechecks it and runs its examples, and replies with an `assert` of a `verified` claim (by the responder's DID) or a `reject` with the reason — validation-as-a-service whose verdict is re-execution, signed and attributable; a **`query`** searches the records by `effects`/`intent_tags`/`terminates` and replies with an `ack` listing the matching content-addresses — discovery over Nova Locutio, the precondition for principle 4. A **`propose`** (which allows refusal) is answered with a **`commit`** — an `apply` commitment, emitted only after the responder *test-runs* the proposal — or a `reject`. Worked flows: [`request-validate`](spec/examples/request-validate.v0.2.json) → [`assert-verified`](spec/examples/assert-verified.v0.2.json); [`query`](spec/examples/query.v0.2.json) → [`ack-query`](spec/examples/ack-query.v0.2.json) (effects `io.console` → `greet`); [`propose`](spec/examples/propose.v0.2.json) → [`commit-apply`](spec/examples/commit-apply.v0.2.json) (apply `double(21)`). The responder also fulfils a received `commit` (run the committed apply → `assert` the result), verifies a `store` payload, acks `delegate`/`retract`, and **capability-gates** `apply`/`propose` — a target declaring required `signature.capabilities` is fulfilled only if the sender is authorized for them (see the delegation-chain verifier below).
- **A real delegation-chain verifier — capabilities are now proven, not just claimed** ([`spec/trust-model.md`](spec/trust-model.md), [`tooling/validator/src/delegation.rs`](tooling/validator/src/delegation.rs)). `nl-validator verify-delegation --capability <cap> --grantee <did> --root <did> --delegations <dir>` decides whether an agent may wield a capability by exhibiting a chain of signed `delegate` tokens back to a root the receiver recognizes *per its own local trust policy* (principles 6, 7 — no central authority). It verifies every token's Ed25519 signature, walks the chain to a recognized root, enforces **attenuation** (no link may widen the grant — capability covering is prefix-on-segments, so `cap:fs/read` covers `cap:fs/read/home` but not the reverse), skips expired tokens, honours bearer tokens (`to: null`), terminates on cycles, and surfaces every `condition` along the chain for the policy layer. It is wired **behind the capability gate**: a responder configured with a `TrustPolicy` (recognized roots + a token pool) fulfils a gated `apply`/`propose` only when the sender can exhibit a valid chain — *listing* the capability string no longer suffices. Demonstrated: a 2-hop attenuated chain ([`spec/examples/delegation/`](spec/examples/delegation/)) — root grants alice `cap:apply`, alice narrows it to `cap:apply/double` for bob — authorizes bob for `cap:apply/double` but rejects `cap:apply/triple`; recognizing alice (not root) collapses it to a verified 1-hop chain
- **Autonomous orchestration — the agent loop driven end to end** ([`tooling/validator/src/orchestrate.rs`](tooling/validator/src/orchestrate.rs)). `nl-validator orchestrate --records <dir> --intent <tag> --arg <value> --seed <s>` runs a full signed conversation: the orchestrator **discovers** a function by intent (`query` → `ack`), **proposes** applying it (`propose` → `commit`), the committer **fulfils** it (`commit` → `assert`), and the orchestrator **verifies** the result by re-running the claim — "assemble, don't write" (principle 4) made autonomous, since the agent never names the function, it finds one. Each `--intent` is a **pipeline stage** — the result of one feeds the next — so the orchestrator *composes* multiple discovered functions. Demonstrated: `--intent arithmetic --arg 21` discovers `double` and confirms `double(21) = 42` (five messages); `--intent arithmetic --intent arithmetic` composes `double` twice, confirming `double(double(21)) = 84` (ten messages), every stage verified
- **Verified orchestration — discover, trust, prove, apply, re-verify** ([`orchestrate_verified`](tooling/validator/src/orchestrate.rs), `nl-validator orchestrate --verify`). The whole thesis in one autonomous run: the orchestrator **discovers** functions by intent (a query returns a *set*), keeps only those whose **signature fits the application** (arity *and* parameter types must accept the arguments — a binary function is no candidate for a unary apply, a list function none for an integer arg, with polymorphic type variables unified consistently, and a **higher-order `fn_ref` argument is itself resolved and type-checked** against the expected function type, so a wrongly-shaped function can't be slipped into e.g. a `foldr` slot), **ranks the surviving candidates by trust** under its *own* local policy over an attestation graph and uses the most-trusted — higher aggregate confidence, then more vertex-disjoint paths, then more distinct attesters (no central authority — principle 7; this replaces a naive "take matches[0]", and if no candidate is trusted the run aborts before anything is touched), **proves** the chosen function's own declared property over the unbounded domain (re-proving it with the SMT + induction + lemma-discovery engine rather than trusting the record's claim), then **applies** it and **re-verifies** the result by re-running (principle 3). This ties the commons, the trust model, the prover, and the message loop into a single flow. Demonstrated live: with a trusted root vouching for `double`, `--verify --intent arithmetic --arg 21` → discover → `trusted` → property `doubles` PROVED → apply → CONFIRMED `21 → 42`; remove the vouching attestation and the identical run ABORTS at the trust gate
- **Static effect inference** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/effects.rs`](tooling/validator/src/effects.rs)). `nl-validator check-effects <record> --body <body>` proves a body's effects ⊆ its declared `signature.effects` *without running it* — the static counterpart to runtime enforcement. It reports SOUND, UNDER-DECLARED (an omitted effect, caught before execution — exit 1), or UNVERIFIABLE (the body directly applies an opaque callee whose effects can't be seen statically). Higher-order arguments' effects belong to the caller (effect polymorphism), so `map`'s declared `[]` stays SOUND. With `--records` it folds in the declared effects of any `fn_ref` callee, so a **composed** body (one that references another commons function) reads SOUND instead of UNVERIFIABLE. Worked: `greet` SOUND `[io.console]`, `double` SOUND `[]`, the `print` body against a no-effects record UNDER-DECLARED, a body applying `greet` by `fn_ref` UNVERIFIABLE bare → SOUND with `--records`
- **The Haskell/TypeScript adapters lift their bodies too** ([`tooling/ingest-common/nl_body.py`](tooling/ingest-common/nl_body.py)). `body_ast_from_ts` parses a TypeScript arrow's expression body with Python's `ast` (TS expression syntax coincides with Python's for the supported subset) and reuses the Python translator — so operators, calls, and member access now translate — `lambda`-wrapped over the arrow's parameters; `body_ast_from_hs` `lambda`-wraps its recognized bare/application bodies. Both now also have `--emit-dir` (records + executable bodies, like Python), and the evaluator gained **float arithmetic** (mixed int/float promotes), so a TS `number` body runs end to end — `nl-ingest-ts --v2 --emit-dir` then `nl-validator run` executes `(n) => n * 3` as `triple(7) = 21`; an HS `ident x = x` likewise (`ident 5 = 5`)

- **Verification over the unbounded domain — an SMT proof backend with re-checkable certificates** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/prove.rs`](tooling/validator/src/prove.rs)). `nl-validator prove <record> --body <body>` is the rung above bounded checking: it translates each `forall` law and the function body to **SMT-LIB 2** (the `Int`/`Bool` fragment — arithmetic, comparisons, boolean connectives, `let`, boolean `case` → `ite`, `self` inlined as a `define-fun`) and asks a solver (z3 by default) whether the *negation* of the law is satisfiable. `unsat` → **PROVED** for all inputs (not a sampled range); `sat` → **REFUTED** with the solver's counterexample (exit 1); out-of-fragment (lists, higher-order) → **UNSUPPORTED**, never silently "proved". The emitted `.smt2` (`--smt-out <dir>`) **is the proof certificate** — any SMT solver re-checks it, so a receiver re-checks rather than trusts (principles 3, 5). Demonstrated: `double`'s `forall n. eq(self(n), add(n,n))` PROVED over every integer; a four-variable commutativity law the bounded enumerator can't cover PROVED outright; `forall n. gt(self(n), n)` REFUTED at `n = 0`.
- **Inductive proof over unbounded recursive structures** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/induct.rs`](tooling/validator/src/induct.rs)). When the first-order pass hits a list law it reports UNSUPPORTED, `prove` falls back to **structural induction**: for `forall xs. P(xs)` over `Lst = nil | cons(Int, Lst)` it discharges a **base** case (`P(nil)`) and a **step** case (assume the IH `P(t)`, prove `P(cons(h, t))`), each an SMT obligation with the list operations (`length`/`append`/`reverse`/`map`/`filter`/…) emitted as z3 `define-fun-rec`s; both `unsat` ⇒ PROVED. `map`/`filter`'s function is modelled as `id` or a single **uninterpreted** symbol, so `forall f xs. length(map(f, xs)) = length(xs)` is proved for *every* `f`. Proved by induction (verified live): `map(id, xs) = xs`, `length(map(f, xs)) = length(xs)`, `length(append(xs, ys)) = length(xs) + length(ys)`. The base + step `.smt2` scripts are the re-checkable certificate.
- **Lemma discovery — inductions that need helper lemmas now close** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/lemmas.rs`](tooling/validator/src/lemmas.rs)). When one unfold + IH stalls — classically `reverse(reverse(xs)) = xs`, whose step needs `reverse(append(as, bs)) = append(reverse(bs), reverse(as))` — the prover selects relevant lemmas from a **curated catalog** of list-algebra laws (`append_nil`, `append_assoc`, `reverse_append`, `length_append`, `map_append`), **proves each one by induction first** (recursively — `reverse_append` rests on `append_assoc` + `append_nil`), then re-runs the stalled obligation with the proved lemmas as universally-quantified axioms. `reverse(reverse(xs)) = xs` is now **PROVED**, discovering `reverse_append` and transitively its two sub-lemmas (verified live; the full proof tree — the goal's base/step plus *each lemma's own base/step* — re-checks, every obligation `unsat` on its own). Sound by construction: a lemma is assumed only after it is itself discharged, so a false law (`reverse(xs) = xs`) stays NOT-PROVED and a true law whose lemma the catalog lacks (`map(f, reverse(xs)) = reverse(map(f, xs))`) stays UNKNOWN — never a false PROVED. Lemma relevance is gated by the goal's *prelude closure* so an unrelated recursive definition can't derail the solver into a timeout. The catalog is the seam where the generalizable follow-on (theory exploration) drops in.
- **Theory exploration — discovering lemmas the catalog doesn't have** ([`spec/evaluation.md`](spec/evaluation.md), [`tooling/validator/src/explore.rs`](tooling/validator/src/explore.rs)). When the curated catalog can't close a goal, the prover *conjectures* fresh lemmas the way QuickSpec / Hipster do: it **enumerates** well-typed terms over the goal's operations (first-order list fragment, within the prelude closure), **tests** each on a fixed battery of inputs, and **buckets terms by equal results** — terms that agree on every test are conjectured equal. Survivors are then **proved by induction** (the same Layer A machinery) before being assumed, so testing is only a filter and soundness still comes from the proof: a conjecture that passes the tests but isn't a theorem is rejected when its induction fails. Demonstrated live: `reverse(append(reverse(xs), ys)) = append(reverse(ys), xs)` — which needs `reverse_append` (catalogued) **and** reverse-involution (*not* catalogued) — is **UNKNOWN under the catalog alone but PROVED once exploration discovers the involution lemma** from scratch; the whole proof tree re-checks. To stay sound and fast, discovered lemmas are added one at a time and the goal is retried with a **minimal** axiom set (catalog + one discovered lemma — piling them all in overwhelms the solver), and proofs are **memoized** so a shared lemma is discharged once. Enumeration and the test battery are fixed (no RNG), so exploration is deterministic (principle 5).
- **Semantic-equivalence proving** ([`tooling/validator/src/equiv.rs`](tooling/validator/src/equiv.rs), `nl-validator equiv`; the node's [`POST /v0/equiv`](tooling/commons-node/commons/equiv.py)). Decides whether two functions compute the same thing — `∀x. f(x) = g(x)` over the unbounded domain — the operable form of the "semantic equivalence vs hash equivalence" open problem (two records hash-different yet behaviorally identical). It reuses the property prover rather than adding a new encoding: when both sides are non-recursive it inlines both into the law `eq(f(x), g(x))` so the operations stay visible to lemma discovery; when one side recurses, that side becomes `self` and the other is inlined. So equivalence is decided with the full SMT + induction + lemma-discovery pipeline — including list laws: `\xs. reverse(reverse(xs)) ≡ \xs. xs` is proved EQUIVALENT, `double-via-add ≡ double-via-mul` PROVED, `double ≢ \n. n+1` returns DISTINCT with a counterexample. A clean DISTINCT comes only from a solver counterexample; a non-closing induction is reported UNKNOWN, never a false DISTINCT (any arity ≥ 1, with at least one side non-recursive — `\a b -> add(a,b) ≡ \a b -> add(b,a)` is PROVED, `add ≢ sub` returns DISTINCT with a counterexample). A **normalization** fast path runs first (`nl-validator normalize`): each body is rewritten to a canonical normal form by meaning-preserving rewrites — α-renaming of bound variables, AC ordering of commutative operators (so `add(a,b)` and `add(b,a)` coincide), constant folding, and identity elimination — and equal normal forms mean equivalent, decided structurally with no solver. Because it needs no induction it also decides the otherwise-unsupported case where *both* functions recurse (renamed/commuted/folded copies of the same recursive function collapse together), and the normal form is a **canonical artifact** with its own `expr_` content-address — the rung above merely picking a representative (the rewrites are strictly sound: no divide-by-zero fold, no absorbing-element rewrite that a non-terminating operand would break). When normalization *can't* reconcile two **both-recursive** single-list functions, a **two-recursive structural induction** is attempted — both bodies become `define-fun-rec`s and `∀xs. f(xs) = g(xs)` is discharged by induction over `xs`. Each body's recursion **stride** (how many elements a `self`-step peels) is read off its AST, and the prover targets the single realigning stride `lcm(stride_f, stride_g)` (`k = 1..6`). `k = 1` decides recursions that align step-for-step and differ only in their element arithmetic (a list-sum written `add(head, self(tail))` ≡ one written `sub(self(tail), neg(head))`, PROVED); a larger stride aligns *misaligned* recursions (a length peeling **one** element per step ≡ one peeling **two**, PROVED at stride 2 — and a **two**-per-step ≡ a **three**-per-step, lcm 6, PROVED at stride 6). The same base cases double as refutation: a satisfiable base case is a concrete short list where the two differ, so genuinely unequal recursive functions return **DISTINCT with a counterexample** (sum ≢ length). Recursions whose alignment period exceeds 6 (e.g. 3-vs-4, lcm 12), recur at a non-constant stride, or need a cross-function lemma stay UNKNOWN — never a false verdict
- **Composition metadata propagation** ([`tooling/validator/src/compose.rs`](tooling/validator/src/compose.rs), `nl-validator compose`). The "composition opacity" open problem: a pipeline of well-described leaves was itself undescribed. `compose f1 f2 …` now derives the composite's metadata from the stages' signatures — **type composability** stage-to-stage (a `nat`-producing stage can't feed a `List`-consuming one), with the composite's input/output types threaded **precisely** through the pipeline's type variables (so `wrap : a → List a ; head : List b → b` composes to the exact `a → a`, not `a → b`), the **union** of effects and capabilities, `always` termination only if every stage is (else `unknown`), and **precise complexity** when every stage carries the v0.3 `cost` metadata — the size threads through the pipeline as a polynomial degree and each stage's cost is substituted at its actual input size, **sound under expansion** where the coarse max under-reports (an expanding stage feeding `O(m²)` work is `O(n⁴)`, not `O(n²)`; a value-measured stage falls back to the coarse bound, never a false `O(1)`), the `cost-basis` line saying which path was taken — so an assembled pipeline is as described as a leaf, the precondition for "assemble, don't write" to yield verifiable artifacts. Worked: `reverse ; length` composes to `List a → nat` (effects `[]`, terminates `always`); `pairs ; pairwise` (an expanding pipeline) is `O(n⁴)` precise where the coarse max says `O(n²)`; `length ; reverse` is NOT composable (a `nat` can't feed a `List` parameter). Unary stages; without `cost` metadata the safe coarse max bound is reported
- **Equivalence clustering** ([`tooling/validator/src/cluster.rs`](tooling/validator/src/cluster.rs), `nl-validator cluster`). Lifts pairwise equivalence to a whole record set: bucket functions by a coarse **signature shape** (so only same-shape functions are ever compared — what keeps it from O(n²) across the set), then run a union-find proving `∀x. f(x) = g(x)` pairwise within each bucket, yielding behavioral-equivalence **classes** each with a canonical representative (the smallest content-address) — deduplication beyond byte-identity (principle 2). Demonstrated: a directory with `\n. add(n,n)`, `\n. mul(2,n)`, `\n. mul(3,n)` clusters the first two into one class (canonical = smaller hash) and leaves the tripling distinct. Scope follows `equiv` (any arity ≥ 1, at least one side of a pair non-recursive), so two mutually-recursive same-shape functions stay separate classes for now
- **The trust model is complete — a policy engine over an attestation graph** ([`spec/trust-model.md`](spec/trust-model.md), [`tooling/validator/src/attestation.rs`](tooling/validator/src/attestation.rs), [`tooling/validator/src/policy.rs`](tooling/validator/src/policy.rs)). On top of the delegation-chain verifier, the last two trust-model pieces are now built. An **attestation** is a signed `assert` whose claim is `<attester> <verb> <subject>` over a closed verb vocabulary (`vouches-for`, `trusts-claims-about`, `distrusts` — resolving the spec's open question on trust verbs). `AttestationGraph` builds the trust graph from a set of messages: it verifies every attestation's signature, drops the targets of authentic `retract`s, and prunes expired edges. The **reference policy engine** (`nl-validator evaluate-trust`) reads a small JSON policy (`trusted_roots`, `max_depth`, `min_distinct_paths`, …) and derives trust — spreading it transitively from the roots and admitting a subject only when ≥ `min_distinct_paths` distinct trusted agents attest to it (the diversity / Sybil mitigation the spec calls for), scoped to a `domain` if given, with `distrusts` overriding a positive path. `nl-validator authorize` is the capability counterpart: it wraps the delegation verifier with the policy's roots and then **enforces** the chain's `conditions` (a condition the policy can't satisfy is refused — finally enforcing, not just surfacing, delegation conditions). Demonstrated: root vouches alice, alice trusts bob for `rust_ingestion` → bob TRUSTED for `rust_ingestion`, UNTRUSTED for `crypto`; a valid delegation chain AUTHORIZED under a permissive policy but UNAUTHORIZED under one that can't satisfy its condition.
- **Richer trust policy — confidence, recency, and true vertex-disjoint diversity** ([`spec/trust-model.md`](spec/trust-model.md), [`tooling/validator/src/policy.rs`](tooling/validator/src/policy.rs)). Three opt-in Sybil gates beyond the distinct-attester count: **confidence-weighting** (`min_confidence` — each attestation's `confidence` propagates from the roots and independent supporters combine by noisy-OR `1 − ∏(1 − cᵢ)`); **recency decay** (`half_life_days` — a vouch's weight decays `0.5 ^ (age / half_life)` from its `issued_at`, so stale endorsements fade); and **vertex-disjoint diversity** (`min_disjoint_paths` — the number of internally vertex-disjoint root→subject paths, computed as a max-flow over the trusted subgraph with unit vertex capacities). The last is the real Sybil measure: two attesters whose chains both funnel through one intermediary count as **one** path, where the old distinct-final-attester count saw two — verified live (a funnel through a shared node is rejected; two independent chains pass).

The "what's next" items from the prior milestones are now done (the `alloc` effect, a TLS + de-chunking net client, the delegation-chain verifier, the first-order SMT backend, the inductive backend, lemma discovery — both the curated catalog and theory exploration — and the trust-model policy engine + attestation graph). **Induction now reaches user-defined recursion too**: a law over a `self`-recursive function (supplied as a body) is proved by encoding that body as its own `define-fun-rec` and inducting as usual — e.g. a user-defined recursive `length` is proved to distribute over `append`, and a false law over it is correctly rejected. **Folds are in the inductive fragment too**: `foldr`/`foldl` are encoded as `define-fun-rec`s over one uninterpreted binary fold function (so a law holds for every `f`), and because `foldl` threads its accumulator the step asserts the induction hypothesis generalized over the non-induction variables — both are proved to distribute over `append`. Building on that prover, the toolchain has since gained **semantic-equivalence** proving (`equiv` / `POST /v0/equiv`) and **clustering** (`cluster` — behavioral-equivalence classes over a record set with a canonical representative), **composition metadata propagation** (`compose` — an assembled pipeline is now described, not opaque), and a **verified agent loop** (`orchestrate --verify`) that discovers a function by intent, filters candidates by signature compatibility (arity + parameter types, including the signature of a higher-order `fn_ref` argument), trust-ranks the survivors under the receiver's local policy, re-proves the chosen function's own property, applies it, and re-verifies the result — with the node serving `/v0/prove` and `/v0/equiv` behind a per-IP rate-limited Caddy edge.

**Remaining frontier:** lifting equivalence and clustering past their remaining scope limit (two mutually-recursive functions whose recursions misalign beyond a small constant stride, or need a cross-function lemma the solver won't invent — multi-argument functions, normalization-reconcilable copies, lockstep recursions, and constant-stride-misaligned recursions whose alignment period (lcm of the two strides) is ≤ 6 — including 2-vs-3 — via the stride-targeted k-step search are now handled, with genuinely-unequal recursive pairs refuted by a short counterexample); extending the canonical *normal form* (`normalize`) past its current rewrite set — a richer decision procedure so classes the rewrites can't reconcile still get a shared normal form, beyond today's α-renaming / AC-ordering / folding; the higher-order list laws now close, including the ones needing an *auxiliary* lemma: **`map` laws over an uninterpreted function discharge** once that function is a quantified parameter (`\f xs -> map(f, reverse xs) ≡ \f xs -> reverse(map f xs)` via `map_append`), and **`filter`/`reverse` commutation** (`filter(p, reverse xs) ≡ reverse(filter p xs)`) — which needs `filter_append` + `append_nil` — is now **PROVED**. The blocker was z3's quantifier instantiation: filter's conditional (`ite(p …)`) makes the lemma trigger order-sensitive, and piling every admissible catalog lemma into one query stalls e-matching. The fix is two-fold — an explicit e-matching **trigger** on each lemma's left-hand side (so instantiation no longer depends on assertion order) plus a **minimal-subset search** (when the full catalog set stalls, retry with the smallest subset that closes, e.g. just `filter_append` + `append_nil`), with a short solver budget for the exploratory attempts. Also beyond the size-ranked conjecture cap; deeper v0.3+ ingestion fidelity; and growing the **verified synthetic training corpus** ([`tooling/corpus/`](tooling/corpus/), now started — a generate→verify→emit pipeline shipping 84 fully-checked examples in three categories — function records (now including sum-typed Maybe/Result and `self`-recursion, both scalar and list-building), agent-loop exchanges, and assembled composition pipelines — with positive and verified-rejected negative cases) into the broad, fluent dataset a model needs. **`self` is now bound in the typechecker and the evaluator** (a recursive body type-checks against its own signature and runs via a recursive closure that re-binds the whole function on each call — correct even under partial application), so a `self`-recursive function passes the full positive gate end to end: `length_rec`/`sum_rec`/`factorial` and the cons-recursive `double_all_rec`/`increment_all_rec`/`append_rec`/`countdown_rec` validate, type-check, and run; the distribution-over-`append` of `length_rec`/`sum_rec`/`product_rec`, the list-building functions' length-preservation (`length(self xs) = length xs`, a law where the recursive function returns a *list*), and a **two-list-parameter** recursive `append`'s length-additivity (`length(self xs ys) = length xs + length ys`, inducting on the first list with the second a spectator) are all proved by induction over the supplied recursive body.

Looking for collaborators on all of the above.

## License

*Novae Linguae* is dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE) ([http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))
- [MIT License](LICENSE-MIT) ([http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in *Novae Linguae* by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.
