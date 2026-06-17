# Synthetic training corpus

A generator for a **verified** Nova Lingua training corpus, addressing the project's standing "training
data" open problem: no model speaks Nova Lingua fluently on day one, and the corpus is part of the
project rather than a follow-on.

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

## Running it

```bash
# build the validator first (the corpus is gated through it)
cargo build --release --manifest-path ../validator/Cargo.toml

python3 gen_corpus.py --out corpus.jsonl     # writes corpus.jsonl + corpus.jsonl.manifest.json
```

The generator **drops any example that fails a verification step and exits non-zero** if it had to — so a
clean run is itself the guarantee that every committed example is fully verified. It is deterministic (the
families enumerate a fixed set, no RNG — principle 5), so the corpus is byte-reproducible.

## Scope (v0.1) and where it grows

Three families today — unary integer functions, binary integer functions, and list functions
(`foldl`-sum, `reverse`, `length`) — 12 examples, 10 with properties proved over the unbounded domain.
This is the seam, not the ceiling: more families (string/Maybe/Result functions, multi-stage compositions
via `compose`, higher-order `map`/`filter` laws now that they discharge), the *Nova Locutio* side (intent →
`request`/`query`/`propose` message exemplars), and negative examples (an artifact paired with the
verification verdict that *rejects* it — equally valuable training signal) all drop in behind the same
"generate → verify → emit" pipeline.
