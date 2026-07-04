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
- **executed** — its worked `examples` are run through its body and the results checked (`nl-validator run`;
  a higher-order record's function-valued (`fn_ref`) argument is resolved from a helper record built alongside it),
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

python3 gen_corpus.py --out corpus.jsonl     # curated corpus.jsonl + corpus.jsonl.manifest.json

# training-scale: ALSO emit the parameterized (combinatorial) function specs to a scratch path
python3 gen_corpus.py --combinatorial --out /tmp/corpus-train.jsonl
```

The generator **drops any example that fails a verification step and exits non-zero** if it had to — so a
clean run is itself the guarantee that every committed example is fully verified. It is deterministic (the
families enumerate a fixed set, no RNG — principle 5), so the corpus is byte-reproducible.

### Two scales: curated vs combinatorial

The default run emits the **curated** corpus (the committed `corpus.jsonl`) — families chosen for breadth
of *shape*, the eval pool and the showcase. `--combinatorial` ALSO emits **parameterized** specs
(`combinatorial_specs`): each hand-authored shape multiplied over a fixed set of constants, operators, and
comparisons — unary / two-step / three-step arithmetic, `map`/`filter`/`count`/predicate over a comparison,
`filter`→`map` pipelines, guarded optionals, range tests, compound (`and`/`or`) predicates, and
**structural recursion** (recursive `map`/`filter`/`count`/`all`/`any`/reduce — the write-hardest shapes,
parameterized) — for the *volume* a fine-tuning dataset needs. It currently yields **3,038 generated
function records (3,264 examples total with the curated set)** across forty template families
(through #38, index recursion — the total `nth` idiom, whose exact gold is leakage-dropped so the family
teaches the shape; a 14B run confirmed it flips `nth` from fail to pass — #39, **strings as data**
(`spec/expressiveness.md` phase 1): split/join/concat/`to_string`/`parse_int` idioms multiplied over
separator and constant sets, incl. the parse-then-`case`-the-`Maybe` shape that replaces `error` — and
#40, **maps & JSON** (phases 2–3): the config-lookup (`get_K_or_D`/`has_K`/`set_K`) and JSON
field-projection (`json_K`, nested `Just(JObj(m))`/`Just(JNum(p))` patterns) idioms multiplied over
key/default sets), every one through the same validate →
typecheck → run gate, and is byte-reproducible. The gate is run on a thread pool (it is subprocess-bound), so a full scaled run takes
~1 minute; output order is preserved, so it stays reproducible and the default curated run is byte-identical
to the serial one. The large combinatorial file is regenerable from the generator, so it is **gitignored,
not committed** — the generator is the artifact. Widen the `_K*` constant sets in `combinatorial_specs` to
scale further (the count grows quadratically in the two-step sets, cubically in the three-step set).

## Negative examples

Every example carries a `polarity`. A **negative** example is a deliberately-wrong artifact paired with
the verifier's **rejection** — the "is this wrong?" signal — and it is valid only if the reference
verifier actually rejects it (the generator drops a negative the verifier accepts, since that would be a
verifier bug or a mislabel, not training signal). So a negative is verified in the dual sense: *verified
to be rejected*, for the stated reason. Today's 14 span eight distinct verifier gates:

| id | wrong because | caught by | verdict |
|----|---------------|-----------|---------|
| `neg_wrong_return_type` | declares `int -> bool`, body returns `int` | `typecheck` | ILL-TYPED |
| `neg_list_op_on_scalar` | reverses an `int` (a list op on a scalar) | `typecheck` | ILL-TYPED |
| `neg_arity_mismatch` | applies `add` to one argument | `typecheck` | ILL-TYPED |
| `neg_cons_onto_scalar` | conses an `int` onto an `int` (needs a `List`) | `typecheck` | ILL-TYPED |
| `neg_refuted_property` | claims `double(n) = n + 1` | `prove` | REFUTED (counterexample) |
| `neg_refuted_commutativity` | claims subtraction is commutative | `prove` | REFUTED (counterexample) |
| `neg_wrong_example` | claims `double(3) = 7` | `run` | EXAMPLE-FAILED |
| `neg_wrong_list_example` | claims `reverse([1,2,3]) = [1,2,3]` | `run` | EXAMPLE-FAILED |
| `neg_schema_invalid` | required `body_hash` field removed | `validate` | SCHEMA-INVALID |
| `neg_under_declared_effects` | prints (`io.console`) but declares no effects | `check-effects` | UNDER-DECLARED |
| `neg_false_claim` | a signed `assert` claiming `double(21) = 43` | `verify-claim` | REFUTED on re-execution |
| `neg_capability_denied` | applies a capability-gated function without the capability | the `respond` capability gate | REJECT `not_authorized` |
| `neg_compose_length_then_reverse` | a `nat` can't feed `reverse`'s `List` parameter | `compose` | NOT-COMPOSABLE |
| `neg_compose_allpositive_then_reverse` | a `bool` can't feed `reverse`'s `List` parameter | `compose` | NOT-COMPOSABLE |

## Scope and where it grows

226 examples today (212 positive, 14 negative), in four `category`s:

- **function** (189) — Nova Lingua function records across **thirty-two families**: unary integer (8, incl.
  `double` / `quadruple` / `decrement` / `abs_val`), binary integer (6, incl. `maximum` / `minimum` /
  `abs_diff`), boolean/predicate (8, incl. `logical_and` / `logical_or` / `logical_xor` / `is_zero` /
  `is_even`), list builtins (3: `sum` / `reverse` / `length`), list-transform (6: `map`/`filter`/`append`
  wrappers — `negate_all` / `square_all` / `keep_positives` / `keep_evens` / `concat` — plus the
  `reverse`-over-`append` law `reverse_concat`), composition (4: `foldl`-product / `length`∘`filter` /
  `sum_of_squares`), **`foldr` aggregations and `List`→`bool` predicates** (5: `all_positive` /
  `any_negative` / `contains_zero` / `all_even` / `sum_foldr`), **refinement-carrying** (7: `divide` /
  `modulo` / `head_of` preconditions, and `post`conditions on `abs_pos` / `inc_spec` / `sum2_spec` plus a
  pre-gated `safe_sub` — all populating `signature.refinements` and **proved against their bodies by
  `check-refinement`** in the gate; the reserved variable `result` names the output), **complexity-carrying**
  (3: `sum2_cost` `O(1)` / `length_cost` `O(n)` / `reverse_naive_cost` `O(n^2)` — each declaring a
  `signature.complexity` bound **and structured `cost`** (`time` + `output_size`) that are **verified
  against its body by `check-complexity`** in the gate, a structural no-solver cost analysis; `reverse_naive`
  is the showcase that time and output size are independent — `O(n^2)` time yet size-*preserving* output —
  which is what `compose` threads through a pipeline and, until now, trusted without proof),
  float (4), Maybe (3) / Result (2), scalar `self`-recursion (5: `length_rec` /
  `sum_rec` / `product_rec` / `factorial` / `triangular`), list-building recursion (6: `double_all_rec` …
  `countdown_rec`), integer algebraic laws (7: associativity / distributivity over `+` *and* `−` / identity
  / annihilation / involution / idempotence), boolean laws (7: associativity, De Morgan for AND and OR,
  idempotence, absorption), order laws (7: `max`/`min` idempotence / commutativity / associativity /
  absorption), **more integer functions** (6: `cube` / `sign` / `clamp` / `in_range` / `is_odd` /
  `is_negative`), **more proved identities** (2: `mul_one` right-identity / `mul_zero` annihilation),
  **more boolean functions** (2: `implies` / `iff`), **more recursion** (7: `member` /
  `count_occurrences` / `take_rec` / `drop_rec` / `repeat_rec` / `pow` / `last_rec`, the last with a
  non-empty refinement), **recursion shapes** (7 generative-`write`-focused: `fib` double recursion,
  `gcd` Euclid two-arg, `sum_digits` div/mod, `range_rec` ascending build, `nth` indexing (refined),
  `concat_lists` nested-list flatten, `keep_positives_rec` recursive filter), **compositional bodies**
  (5: `max_of_list` / `min_of_list` folds with a builtin seeded by the head (refined), `count_between`
  inline-predicate filter, `clamp_all` inline-lambda map, `sum_of_cubes` compound fold step),
  **more compositional bodies** (9 generative-`write`-focused: `average_two` / `abs_diff` /
  `sum_squares_two` / `square_diff` two-arg arithmetic, `at_least_zero` clamp-from-below, `triple_all`
  map, `keep_negatives` filter, `count_negatives` `length`∘`filter`, `sum_evens` filter→fold pipeline),
  **more recursion** (3: `mult_rec` multiply-by-repeated-addition, `pow2` doubling recursion,
  `max_list_rec` recursive non-empty-list maximum (refined)),
  **variant-consuming** (7 generative-`write`-focused: the first records to *pattern-match on* sum types —
  `unwrap_or` / `is_some` / `maybe_double` over `Maybe`, `unwrap_result` / `result_to_maybe` over `Result`
  — plus the constructors `predecessor` (→ `Maybe`) and `to_result_nonneg` (→ `Result`)),
  **nested higher-order + multi-clause case** (6: `count_even_positives` compound-predicate filter,
  `doubled_evens` map∘filter, `sum_doubled` fold∘map, `any_even` not∘null∘filter, and the nested-`case`
  chains `grade` (4-way threshold) and `compare_to` (3-way comparison)),
  **higher-order** (10: `map_with` / `filter_with` / `foldl_with` / `foldr_with` /
  `apply_to` / `twice` / `compose2` / `all_with` / `any_with` / `count_with` — records whose
  *type* takes a function argument, run end to end with the function supplied as an `fn_ref` to a helper
  record resolved from the run directory; the grader can render the fn_ref argument by the helper's name),
  **strings** (13, `spec/expressiveness.md` phase 1 — the first records over string *data*:
  `str_len` (Unicode scalar count) / `wrap_parens` / `contains_comma` (needle-first) /
  `count_fields` / `second_field` / `split_words` (split keeps empties) / `comma_join` / `show_int` /
  `render_ints` (`str_join`∘`map to_string`) and the parse quartet `parse_int_maybe` /
  `parse_or_zero` / `is_int_string` / `parse_and_double` — `parse_int`'s `Maybe` constructed AND
  consumed by `case`, the totality idiom that replaces `error`),
  **maps + JSON** (9, `spec/expressiveness.md` phases 2–3 — the first records over dynamic key-value
  data and JSON-as-data: `lookup_int` / `port_or_default` (the config-lookup idiom) / `key_count` /
  `key_list` (sorted, deterministic) / `store_one` (build from `map_empty`) / `drop_key` (absent key
  is a no-op) over maps, and `is_valid_json` / `canonical_json` (`render_json ∘ parse_json` IS
  JCS canonicalization) / `json_port` (the GW1 practical form — nested `case` over `Just(JObj(m))`
  then `Just(JNum(p))`) over JSON; examples carry real `map` values),
  and **provenance** (2: `quadruple_derived` `derived_from`
  doubling, `negate_v2` `supersedes` a `0 − n` implementation). **56 properties are proved over the
  unbounded domain**, including the `filter`/`reverse` commutation and the `reverse`-over-`append`
  antihomomorphism (both via lemma discovery), `filter` idempotence (direct induction), and the recursion
  families' laws by induction over the supplied body. Sum-typed (Maybe/Result) functions construct their
  variant result with a computed payload (`Just(a / b)`, `Err(b)`) and verify by schema + typecheck + run
  (sum types are opaque to the prover). The recursion families call themselves via `self` — bound in both
  the typechecker and the evaluator — and prove laws like distribution over `append` and length-preservation
  by induction over the supplied recursive body. (Includes 10 of the negatives — see the table above.)
- **exchange** (20) — Nova Locutio signed agent-loop exchanges spanning all nine speech acts:
  `request`/`apply` → `assert` (incl. applies over a *list* argument, a *boolean* result, a `cube`
  scalar, and a **recursive** `member` whose claim re-run binds `self`, all `verify-claim` CONFIRMED),
  `request`/`validate` → `assert` (scalar *and* list functions),
  `request`/`store` → `ack`, `propose` → `commit` (incl. over a list function), `commit` → `assert`,
  `delegate` → `ack`, `retract` → `ack`, and `query` → `ack` (by `list` and by `refinement` tag), plus 2
  negatives (a signed-but-false claim, a capability-denied apply).
- **transcript** (2) — multi-turn signed exchanges: the agent **discovers** a function (`query` → `ack`)
  then **uses the discovered content-address** in a threaded follow-up turn — `discover_then_apply` (→
  asserts `double(21) = 42`, re-runs true) and `discover_then_validate` (→ asserts `reverse` verified).
  Principle 4 made multi-turn; a transcript is valid only if the ack actually lists the target the next
  turn uses, and the whole chain is threaded by `in_reply_to`.
- **composition** (15) — assembled pipelines with the composite metadata `nl-validator compose` derives
  from the stages' signatures, up to a four-stage `keep_positives;square_all;reverse;sum` → `int` (and
  pipelines over the recursion-based list functions, e.g. `increment_all_rec;sum`, `keep_positives_rec;sum`,
  `concat_lists;length`), plus 2 negatives (a
  `nat` and a `bool`, each unable to feed a `List` parameter). The category for "assemble, don't write"
  (principle 4).

This is the seam, not the ceiling, all behind the same "generate → verify → emit" pipeline: more breadth
within each family, richer multi-turn transcripts, and more negative cases.
