# Nova Lingua model-evaluation harness

Does exposure to the verified corpus actually let a model **read, write, and assemble** Nova Lingua? The
corpus is built on the bet that it does — this harness is the metric that tests the bet, and it grades
with the reference tooling rather than a human or an LLM judge. Every score is a `nl-validator` verdict,
so it inherits the project's verified-by-default property: a "correct" answer is one that *validates,
type-checks, runs, or composes*, full stop.

It's the minimal first step of the headline milestone — *a model that speaks the languages* — and the
metric that tells you whether scaling the corpus (e.g. ingesting real code) is worth it before you spend
the compute.

## Baseline results (2026-06-20, `claude-opus-4-8`, in-context)

The first real run produced a clear, actionable answer to the bet.

| run | write | read | assemble | total |
|-----|------:|-----:|---------:|------:|
| stock prompt (under-specified dialect) | 18% | 51% | 100% | **37%** |
| + surface conventions stated in the prompt | 94% | 100% | 100% | **97%** |

The 37% massively understated competence: graded against the reference tooling, nearly every failure was a
**surface-dialect mismatch, not a reasoning error**. The model computed correct answers and wrote
semantically-correct programs, but reached for mainstream priors instead of Nova Lingua's surface forms —
call-parens application (`f(x, y)`) instead of juxtaposition (`f x y`), curried lambdas (`\a -> \b ->`)
instead of multi-binder (`\a b ->`), Haskell-style `case … of p -> e` instead of brace/arrow
`case … of { p => e }`, and bare integer literals (`42`, parses as `nat`) instead of `int(42)`. The stock
system prompts compounded it by *showing non-parsing examples* (e.g. `add(n, n)`, `and or xor not`); the
few-shot shots, drawn from real corpus bodies, were correct, so the model got contradictory signal.

Stating the five conventions explicitly in `WRITE_SYSTEM` / `READ_SYSTEM` (juxtaposition application,
multi-binder lambdas, infix operators, `int(N)` literals, brace/arrow `case`, `nil`/`cons`, variant
constructors) lifted the score to **97%** — **105 tasks fixed, 0 regressions**. The last few misses were a
model-emitted inline-backtick wrapper (now stripped in `strip_answer`) and the `nil` empty-list form (now
stated), leaving the model's effective semantic competence at ~100% on this corpus.

**Takeaway:** the corpus/exposure bet is validated. The model already has the semantics; what it needs is
exposure to the exact surface forms — which is precisely what stating the conventions (or scaling the
corpus / fine-tuning) provides. The gap is dialect, not reasoning. *(Cost note: a full-pool run measures
~$1, and **effort is not the cost lever** — measured high ≈ medium (~$1 each), because these short
single-answer tasks need almost no thinking at any effort. Cost is driven by prompt length (stating the
conventions roughly triples input tokens) and by how many runs you do; the historical ~$30 was a *day of
many runs* (the on/off/shots sweep + iteration), not one expensive run. A real run is now gated behind an
explicit `--model` (the free oracle is the default), and the cost control is running sparingly, not
subsetting. See Running.)*

### Does the corpus *alone* teach the dialect? (`--conventions off`)

The 97% above states the conventions in the prompt. The sharper question the corpus is built to answer is
whether the **examples alone** teach the dialect, with the rules removed entirely. `--conventions off`
drops the convention block and leaves only the few-shot examples drawn from the corpus; `--shots N` scales
how many. Run on `claude-opus-4-8` over the corpus's write/read/assemble pool (179 tasks):

| condition | write | read | assemble | total |
|-----------|------:|-----:|---------:|------:|
| conventions **on**, 3 shots | 98.9% | 98.8% | 100% | **98.9%** |
| conventions **off**, 3 shots | 37.5% | 89.3% | 100% | **64.2%** |
| conventions **off**, 10 shots | 71.6% | 98.7% | 100% | **85.5%** |

The two skills come apart. **Reading** recovers almost entirely from examples alone — 89% at 3 shots,
99% at 10 — so comprehension of the surface forms is learnable from exposure. **Writing** is the hard
half: 37.5% with 3 examples and no rules (right on the original stock-prompt 37% baseline), rising to
71.6% at 10 shots but still well short of the 99% the stated rules buy. So the corpus, as few-shot
context, teaches comprehension readily and generation only partially — more examples help, but generation
is where explicit conventions (or, the corpus bet, *enough* examples via fine-tuning rather than a handful
in-context) still pay off most. This is the quantitative case for scaling the corpus, and for `write`
being the metric to watch as it grows.

> The committed `results.jsonl` is the `--oracle` grader self-test (100%). Real-model runs above were
> written to scratch paths; re-run `--model claude-opus-4-8 [--conventions off] [--shots N]` to reproduce.

## Surface vs. semantic: measuring the dialect tax directly

The baseline finding — *failures are dialect, not reasoning* — was an interpretation of the verdicts. The
grader now **measures it** instead of leaving it to read-off. Every `write` and `read` verdict carries two
results:

- **`pass`** — *surface-exact*: the answer graded exactly as written.
- **`semantic_pass`** — the answer graded again after `repair_surface()`, a set of mechanical,
  value-preserving rewrites that normalize the known dialect deviations to Nova Lingua's surface forms:
  call-parens → juxtaposition (`max(a, b)` → `max(a)(b)`), bare integers → `int(N)`, curried lambdas
  (`\a -> \b ->`) → multi-binder (`\a b ->`), and `[]` → `nil`.

Every repair is a pure *notational* rewrite — it changes spelling, never the computed value or a number's
magnitude. The safety property that makes the metric trustworthy: a botched rewrite produces a string that
fails to parse / typecheck / run, so it only ever *lowers* `semantic_pass`; it can never turn a wrong
answer into a passing one (wrapping `5` as `int(5)` fixes an encoding, never makes a wrong number right).
So **`semantic_pass` is a conservative lower bound on "right modulo dialect"**, and the gap
`semantic_pass − pass` is a measured floor on the surface-dialect tax — the exact quantity the baseline
asserted by hand. The harness prints both columns; `assemble` has no surface dimension (answers are exact
function names) so its two columns coincide. The oracle scores 100% on both, and the test suite asserts
the negative direction too: a genuinely wrong value fails `semantic_pass` as well.

### Measured (2026-06-21, `claude-opus-4-8`, medium effort, 272-task pool)

The verdict split earns its keep on `--conventions off`, where the model makes the dialect errors repair
can catch:

| condition | write surface → semantic | total surface → semantic |
|-----------|:------:|:------:|
| conventions **on** | 99.3% → 99.3% | 98.9% → 98.9% (no gap — near-perfect, the few misses are genuine) |
| conventions **off** | **26.7% → 50.4%** | **58.1% → 71.3%** |

Mechanical dialect repair **nearly doubles** conventions-off `write` (36→68 of 135) and lifts the total by
13 points — so **~half of all write failures are dialect-only, not reasoning errors**. `read` barely moves
(88.0% → 91.2%): comprehension of the surface forms was already robust; `write` is where the dialect tax
lives. This is the "failures are dialect, not reasoning" thesis turned from a hand-read into a number.

## Task shapes

All tasks are drawn from the verified corpus (`../corpus/corpus.jsonl`), so the ground truth is itself
machine-checked. **Higher-order records are now in the `write` pool**: an example whose worked argument is
a function-valued (`fn_ref`) reference carries its helper record + body in the corpus (`views.helpers`),
which the grader materializes into the run directory and links via `run --body … --records …` so the
model-written body executes end-to-end (the model writes the body from the intent + type; the fn_ref
argument is rendered by the helper's name). They stay out of the `read` pool — the helper is opaque by
address, so the output isn't predictable by hand.

- **write** — given an intent, a type signature, and worked examples, the model emits a function *body*
  in the surface syntax. Graded by `parse-body` → `typecheck` (does it have the declared type?) → `run`
  (do the worked examples execute correctly against it?). A task passes only if the body type-checks
  **and** runs every example.
- **read** — given a body and an input, the model predicts the output value. Graded by canonicalizing its
  value (`parse-value` → `unparse-value`) and comparing to the example's true result.
- **assemble** — given a goal and a candidate set of functions (correct stages + distractors), the model
  picks an ordered pipeline. Graded by `compose` (does the chosen pipeline actually type-compose?).

## Running

```sh
# 1. Self-test the GRADER with no API access — a perfect 'oracle' model must score 100%. This is the
#    DEFAULT (no --model needed); run it first. If it isn't 100%, the grader is rejecting valid answers.
python3 eval_harness.py                  # equivalently: --oracle

# 2. Run a REAL model — BILLS ANTHROPIC_API_KEY, *outside* any Pro/Max subscription. A real run happens
#    only with an explicit --model, and the harness prints a cost warning first. A full-pool run measures
#    ~$1; cost is driven by prompt length and run count, NOT effort (high ~ medium for these short tasks).
#    The score is a benchmark over the FULL pool, so don't sample to save money — just run it sparingly.
python3 eval_harness.py --model claude-opus-4-8
python3 eval_harness.py --model claude-opus-4-8 --effort high   # ~same cost as medium; marginally higher score

# Experiment knobs (apply to oracle or real runs)
python3 eval_harness.py --conventions off              # drop the rules; few-shot examples only
python3 eval_harness.py --conventions off --shots 10   # …and scale the number of shots
# (--tasks {write,read,assemble} and --limit N exist for harness debugging only — a score must cover the
#  whole pool to be comparable, so don't use them to cut cost.)
```

Output is a per-kind pass-rate summary with two columns — **surface** (`pass`, graded as written) and
**semantic** (`semantic_pass`, graded after mechanical dialect repair; see above) — plus `results.jsonl`
(every task's prompt, output, and full verdict, including the `repaired` flag).

## Why the oracle matters

`OracleModel` returns each task's known-correct answer. It exists to verify the **grader**: a model that
always answers correctly must score 100%, which proves the validate/typecheck/run/compose pipeline
accepts valid artifacts. The negative-control tests prove the grader also *rejects* wrong answers, so the
100% isn't a grader that passes everything. The grader was validated this way before any real model ran —
and the oracle immediately earned its keep: it surfaced a surface-syntax round-trip bug (`int(N)` body
literals weren't parsed back to literals), which is now fixed in the validator. Both run with no key:

```sh
python3 -m unittest discover -s tests
```

## Scope (v0.1)

The eval is **in-context / few-shot** — it measures whether a capable model, shown the format and a few
examples, can produce *valid* Nova Lingua; it is not a fine-tuning loop. That's deliberate: in-context
performance is the cheapest signal for whether the corpus representation works at all, and it's the
metric that should gate the decision to scale corpus generation. The model client is the only
provider-specific piece (`model_client.py`, Anthropic SDK); the grader is provider-agnostic.
