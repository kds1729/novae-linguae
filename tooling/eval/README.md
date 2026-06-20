# Nova Lingua model-evaluation harness

Does exposure to the verified corpus actually let a model **read, write, and assemble** Nova Lingua? The
corpus is built on the bet that it does — this harness is the metric that tests the bet, and it grades
with the reference tooling rather than a human or an LLM judge. Every score is a `nl-validator` verdict,
so it inherits the project's verified-by-default property: a "correct" answer is one that *validates,
type-checks, runs, or composes*, full stop.

It's the minimal first step of the headline milestone — *a model that speaks the languages* — and the
metric that tells you whether scaling the corpus (e.g. ingesting real code) is worth it before you spend
the compute.

## Task shapes

All tasks are drawn from the verified corpus (`../corpus/corpus.jsonl`), so the ground truth is itself
machine-checked.

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
