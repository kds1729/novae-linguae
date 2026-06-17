# Synthetic training corpus

A generator for a **verified** training corpus spanning **both** languages — *Nova Lingua* (function
records) and *Nova Locutio* (agent-loop message exchanges) — addressing the project's standing "training
data" open problem: no model speaks them fluently on day one, and the corpus is part of the project
rather than a follow-on. Each example carries a `modality` of `nova_lingua` or `nova_locutio`.

The distinguishing constraint is the project's own thesis — *verified by default*. A training corpus full
of plausible-but-wrong artifacts teaches a model to emit plausible-but-wrong artifacts. So every example
here is **correct by construction and then checked by the reference tooling**: each generated function
record is

- **schema-validated** against [`function-record.v0.2.schema.json`](../../spec/function-record.v0.2.schema.json),
- **type-checked** — its body is confirmed to have its declared `signature.type` (`nl-validator typecheck`),
- **executed** — its worked `examples` are run through its body and the results checked (`nl-validator run`),
- **proved** — where it declares algebraic `properties`, each is proved over the *unbounded* domain with
  the SMT + induction + lemma-discovery prover (`nl-validator prove`), or bounded-checked against the
  examples (`nl-validator check-properties`).

Only artifacts that pass all of the above enter the corpus, and each example ships its verification
verdicts, so a learner can train on the "is this right?" signal too — not just the artifact.

## What an example looks like

Each line of `corpus.jsonl` pairs a natural-language **intent** with several **views** of the same
function, so a model can learn the bidirectional NL ↔ Nova Lingua mapping and the verification behind it:

```jsonc
{
  "id": "double",
  "intent": "Double a number.",            // the natural-language task
  "summary": "Returns n + n.",
  "tags": ["arithmetic", "linear"],
  "views": {
    "surface_type": "int -> int",          // canonical surface syntax (nl-validator unparse-type)
    "surface_body": "\\n -> n + n",        // canonical surface syntax (unparse-body)
    "record": { ... },                     // the full self-describing v0.2 function record (JSON AST)
    "body":   { ... },                     // the executable body-expression AST
    "examples": [ ... ],                   // worked examples as value ASTs
    "properties": [ { "name": "doubling", "expr": { ... } } ]   // algebraic laws (predicate ASTs)
  },
  "verification": {
    "schema_valid": true,
    "well_typed": true,
    "examples_passed": "3/3",
    "bounded_check": ["UNVERIFIABLE"],     // check-properties verdicts (self-referencing laws need the prover)
    "proofs": [ { "name": "doubling", "verdict": "PROVED" } ]    // prove verdicts over the unbounded domain
  }
}
```

Properties are stated as a *different* expression than the body where possible (double's body is
`add(n, n)` but its law says `self(n) = 2·n`), so the proof is non-trivial.

A **Nova Locutio** example (`"modality": "nova_locutio"`) is a real **signed agent-loop exchange** — a
natural-language intent paired with the message a sender would emit and the reply the responder
(`nl-validator respond`) actually produces:

```jsonc
{
  "id": "locutio_apply_double",
  "modality": "nova_locutio",
  "intent": "Ask an agent to compute double of 21.",
  "views": {
    "speech_act": "request",
    "request":  { "kind": "request", "body": { "action": "apply", "target": "fn_…", "args": [ … 21 … ] },
                  "from": "did:nova:…", "signature": "ed25519:…", "hash": "msg_…", ... },
    "reply":    { "kind": "assert",  "body": { "claim": { … eq(double(21), 42) … } },
                  "from": "did:nova:…", "signature": "ed25519:…", "in_reply_to": "msg_…", ... },
    "reply_act": "assert"
  },
  "verification": {
    "request_schema_valid": true,
    "reply_schema_valid": true,
    "threaded": true,                  // reply.in_reply_to == request.hash, addressed back to the sender
    "outcome": "CONFIRMED"             // the assert's claim re-ran true via verify-claim (principle 3)
  }
}
```

The verification is the agent loop's own: a `request`/`apply` is answered with an `assert` whose claim
**re-runs true** (`verify-claim`), a `propose` is answered with a `commit` only after the responder
test-runs it, and a `query` is answered with an `ack` of the matching content-addresses. Identities are
deterministic (fixed seeds), so the signed exchanges are byte-reproducible.

## Running it

```bash
# build the validator first (the corpus is gated through it)
cargo build --release --manifest-path ../validator/Cargo.toml

python3 gen_corpus.py --out corpus.jsonl     # writes corpus.jsonl + corpus.jsonl.manifest.json
```

The generator **drops any example that fails a verification step and exits non-zero** if it had to — so a
clean run is itself the guarantee that every committed example is fully verified. It is deterministic (the
families enumerate a fixed set, no RNG — principle 5), so the corpus is byte-reproducible.

## Negative examples

Every example carries a `polarity`. A **negative** example is a deliberately-wrong artifact paired with
the verifier's **rejection** — the "is this wrong?" signal — and it is valid only if the reference
verifier actually rejects it (the generator drops a negative the verifier accepts, since that would be a
verifier bug or a mislabel, not training signal). So a negative is verified in the dual sense: *verified
to be rejected*, for the stated reason. Today's five:

| id | wrong because | caught by | verdict |
|----|---------------|-----------|---------|
| `neg_wrong_return_type` | declares `int -> bool`, body returns `int` | `typecheck` | ILL-TYPED |
| `neg_refuted_property` | claims `double(n) = n + 1` | `prove` | REFUTED (counterexample) |
| `neg_wrong_example` | claims `double(3) = 7` | `run` | EXAMPLE-FAILED |
| `neg_false_claim` | a signed `assert` claiming `double(21) = 43` | `verify-claim` | REFUTED on re-execution |
| `neg_capability_denied` | applies a capability-gated function without the capability | the `respond` capability gate | REJECT `not_authorized` |

## Scope and where it grows

44 examples today (38 positive, 6 negative), in three `category`s:

- **function** (30) — Nova Lingua function records across nine families (unary integer, binary integer,
  boolean/predicate, list, list-transform: `map`/`filter`/`append`, composition: `foldl`-product /
  `length`∘`filter`, float: `square_f` / `double_f`, Maybe: `safe_div` / `first`, and Result:
  `checked_div` / `checked_sub`), 13 with properties proved over the unbounded domain, plus 3 negatives.
  The sum-typed (Maybe/Result) functions construct their variant result with a computed payload
  (`Just(a / b)`, `Err(b)`); sum types are opaque to the prover, so they verify by schema +
  typecheck + run rather than proof.
- **exchange** (11) — Nova Locutio signed agent-loop exchanges (`request`/`apply` → `assert` ×2 both
  `verify-claim` CONFIRMED, `request`/`validate` → `assert`, `request`/`store` → `ack`, `propose` →
  `commit`, `commit` → `assert` (CONFIRMED), `delegate` → `ack`, `retract` → `ack`, `query` → `ack`),
  plus 2 negatives (a signed-but-false claim, a capability-denied apply).
- **composition** (3) — assembled pipelines with the composite metadata `nl-validator compose` derives
  from the stages' signatures (`reverse;length` → `nat`, `negate_all;reverse` → `List int`), plus 1
  negative (`length;reverse` — a `nat` can't feed a `List` parameter, so the pipeline does not compose).
  The category for "assemble, don't write" (principle 4).

This is the seam, not the ceiling, all behind the same "generate → verify → emit" pipeline: more Nova
Lingua families; longer/branching `compose` pipelines; full multi-turn orchestrated transcripts via
`orchestrate`; and more negative cases.
