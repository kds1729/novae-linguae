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

### 7. Minimal orthogonal vocabulary; canonical form

Human languages accumulate synonyms — historical accident, register, politeness, aesthetic variation, social signaling, recall redundancy. None of those forces apply to AI. *Novae Linguae* therefore enforces:

- **One concept, one symbol.** Each primitive in *Nova Lingua* is unique in what it means. Each speech act in *Nova Locutio* is unique in what it does. No synonyms in the primitive vocabulary.
- **Canonical normalized form.** For any semantic intent, exactly one syntactic representation. "Formatting" and "style" do not exist; diffs are always semantic, never cosmetic. Two agents producing semantically identical artifacts produce identical bytes — and identical hashes, reinforcing principle 2.

One nuance worth being precise about: things that *look* like synonyms can carry distinct information and should be preserved. `map` (pure) and `for_each` (effectful) look similar; their effect signatures make them genuinely different operations. *Request* and *propose* both invite action; one expects compliance, the other allows refusal as a normal outcome. The rule: **eliminate redundancy that carries no information; preserve distinctions even when they look superficially similar.**

A bonus this gives us: it shrinks the training problem. When there is exactly one right answer for any expression, models learn the language from far less data than a human language with ambiguity. Minimality is itself a training-efficiency multiplier.

### 8. The runtime is AI-targeted too

The language family is half the story. The runtime is the other half, and the same principle applies — design for AI strengths:

- **Replayable execution** by construction (principle 5).
- **Structured trace output** AI can ingest natively, not stack traces designed for human eyes.
- **Adaptive optimization** driven by execution profiles AI can analyze and tune.
- **Memory layout, scheduling, and concurrency strategy** chosen per workload, not by a one-size runtime default.

---

## Nova Lingua — the programming language

*Nova Lingua* is the language AI agents write programs in. A minimal sketch of what a function record looks like:

```json
{
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
    "identity:    map(id, xs) == xs",
    "composition: map(f, map(g, xs)) == map(f . g, xs)"
  ],
  "intent_tags": ["transform", "list", "elementwise"],
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
  "kind": "request",
  "hash": "msg_e7a2…",
  "from": "did:agent:claude-7f3a",
  "to":   "did:agent:executor-9b22",
  "in_reply_to": null,
  "body": {
    "action": "apply",
    "fn":     "fn_3a9b…",
    "args":   ["double", [1,2,3]]
  },
  "constraints": {
    "capabilities":   [],
    "budget_tokens":  1000,
    "deadline_ms":    5000
  },
  "signature": "ed25519:…"
}
```

And an assertion carrying proof:

```json
{
  "kind": "assert",
  "hash": "msg_f1b3…",
  "from": "did:agent:verifier-2c91",
  "subject": "fn_3a9b…",
  "claim":   "satisfies: property('identity')",
  "evidence": "proof_hash_4d8e…",
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

AI agents are first-class contributors. `CONTRIBUTING.md` will specify the protocol once the schemas stabilize.

---

## Open problems we already know about

These are tractable iteratively. The principles above are not.

- **Bootstrapping the commons.** Empty on day one. Strategy: aggressive ingestion of existing ecosystems, lifted into the record form. The open-source contribution model is the primary plan.
- **Semantic equivalence vs hash equivalence.** Two functions can be hash-different but behaviorally identical. Need clustering, canonical forms, equivalence proofs.
- **Composition opacity.** Even when every leaf is well-described, a pipeline of twenty leaves has emergent behavior. Metadata must propagate upward through compositions automatically.
- **Discovery cost.** Querying a million-function commons has its own context-window cost. Need typed search plus an embedding index.
- **Training data.** No model speaks *Nova Lingua* or *Nova Locutio* fluently on day one. A synthetic-corpus and fine-tuning plan is part of the project, not a follow-up.
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

## Status

**Day zero.** This document is the first artifact. Looking for collaborators on:

1. The shared record schema for functions and messages.
2. The *Nova Lingua* surface form and compiler reference.
3. The *Nova Locutio* speech-act vocabulary and wire format.
4. The ingestion strategy for bootstrapping the commons.

## License

*Novae Linguae* is dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE) ([http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))
- [MIT License](LICENSE-MIT) ([http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in *Novae Linguae* by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.
