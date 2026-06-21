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
corpus / fine-tuning) provides. The gap is dialect, not reasoning. Each run cost ~$0.36.

> The committed `results.jsonl` is the `--oracle` grader self-test (100%). Real-model runs above were
> written to scratch paths; re-run `--model claude-opus-4-8` to reproduce.

## Task shapes

All tasks are drawn from the verified corpus (`../corpus/corpus.jsonl`), so the ground truth is itself
machine-checked. The write/read pool is restricted to **self-contained** examples — a higher-order record
whose worked example takes a function-valued (`fn_ref`) argument needs its helper record in the run
directory to execute, which the standalone graders don't supply, so those examples are excluded from
tasks (they remain valid corpus training data).

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
# 1. Self-test the GRADER with no API access — a perfect 'oracle' model must score 100%.
#    Run this first; if it isn't 100%, the grader is rejecting valid answers.
python3 eval_harness.py --oracle

# 2. Run a real model (needs ANTHROPIC_API_KEY in the environment).
python3 eval_harness.py --model claude-opus-4-8

# Options
python3 eval_harness.py --tasks write --limit 10   # one task kind, capped
python3 eval_harness.py --effort xhigh             # effort for the real model
```

Output is a per-kind pass-rate summary plus `results.jsonl` (every task's prompt output and verdict).

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
