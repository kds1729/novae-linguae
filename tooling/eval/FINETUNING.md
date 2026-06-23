# Fine-tuning a model to speak Nova Lingua — plan

This is the real test of the corpus bet and the headline milestone ("a model that speaks the languages").
In-context few-shot has hit its ceiling as a proxy: with the surface conventions *stated* a capable model
scores ~99%, but **conventions-off** (the corpus-teaches-the-dialect condition) `write` sits at ~26%
surface / ~50% "modulo dialect" — three to ten few-shot examples can't teach ~29 families. The corpus was
always meant to be **trained on**, not shown in-context. We now have the runway for that: a verified,
byte-reproducible generator that produces a training-scale dataset on demand.

Everything below the "money gate" line is free/local and already built or buildable without spend. The
gate itself — choosing a provider and running the fine-tune + its eval — costs money and is the one thing
that needs sign-off (and the API key, per the project's cost rule).

## 1. Data — ready now (free)

`gen_corpus.py --combinatorial` emits a verified corpus (currently **2,494 generated function records**,
2,692 examples; scalable to 10k+ by widening the `_K*` sets). `export_finetune.py` converts it to
chat-format SFT pairs — **identical to the eval prompts**, so the model trains on exactly what it's graded
on:

```sh
python3 ../corpus/gen_corpus.py --combinatorial --out /tmp/corpus-train.jsonl
python3 export_finetune.py --corpus /tmp/corpus-train.jsonl --out /tmp/sft.jsonl
```

Each line is `{"kind", "messages": [system, user, assistant]}`: system = the surface conventions, user =
the task (write: intent+type+examples; read: body+input), assistant = the verified gold. Every pair is
correct-by-construction (the corpus gate already ran validate→typecheck→run). Format is the de-facto SFT
shape accepted by OpenAI / Together / Fireworks / Axolotl / TRL.

## 2. Train/test integrity (free)

Hold out a **disjoint** evaluation set to avoid leakage:
- Train on the **combinatorial** corpus; evaluate on the **curated** `corpus.jsonl` (different
  shapes/constants), OR generate a held-out combinatorial slice with constants excluded from training.
- Dedup across splits by body-AST/content hash (the corpus is content-addressed, so this is exact).

## 3. Base model + method (the choice: OpenAI `gpt-4o-mini`)

This is a **narrow DSL**, not open-ended language — a small model plus enough verified examples should learn
the surface dialect; a frontier model is unnecessary. The pick is **OpenAI managed supervised fine-tuning
of `gpt-4o-mini`** for three concrete reasons: the exported SFT is already OpenAI's exact chat format (zero
reformatting), it's the cheapest turnkey self-serve fine-tuning path, and the eval client is trivial
(`OpenAIModel` is wired into `model_client.py`; the harness routes any `gpt*`/`ft:*` id to it). Train
**conventions-OFF** — that's the bet: the model must internalize the dialect from the examples, not from
rules spelled out in the prompt.

## 4. Runbook

All steps are scripted; only the last two bill.

```sh
# (free) build a training-scale corpus and the conventions-OFF SFT training file
python3 ../corpus/gen_corpus.py --combinatorial --out /tmp/corpus-train.jsonl
python3 export_finetune.py --corpus /tmp/corpus-train.jsonl --conventions off --kinds write,read --out /tmp/sft-train.jsonl

# (free) a held-out eval file is NOT needed for training — eval is the curated corpus.jsonl (disjoint
# shapes), graded by nl-validator, so there is no train/test leakage.

# (billed) baseline: how does the BASE model do, in-context, before any training?
python3 eval_harness.py --model gpt-4o-mini --conventions off    # ~$1
python3 eval_harness.py --model gpt-4o-mini --conventions on     # ~$1 (the in-context ceiling for this model)

# (billed) fine-tune on OpenAI, then evaluate the tuned model conventions-OFF
openai api fine_tuning.jobs.create -t /tmp/sft-train.jsonl -m gpt-4o-mini   # or the dashboard / SDK
python3 eval_harness.py --model ft:gpt-4o-mini:<job-suffix> --conventions off   # ~$1 — the headline result
```

**Success criterion:** the fine-tuned model's **conventions-off** `write` should rise from the base model's
conventions-off baseline toward what *stated conventions* buy it in-context — i.e., training internalized
the dialect. Watch `write` (the gap); `read` is already strong. The surface-vs-semantic split still applies.

---

## The money gate (needs the OpenAI key + go-ahead)

Everything above the runbook's "(billed)" lines is done and free. The billed steps:
1. **Base-model eval** (2 runs, ~$1 each on `gpt-4o-mini`) — establishes the before.
2. **The fine-tune job** — OpenAI managed SFT on ~5k short pairs; rough order single-digit-to-low-tens of
   dollars (confirm against current OpenAI fine-tuning pricing at run time).
3. **Tuned-model eval** (~$1) — the headline after/before comparison.

Total rough order **~$10–25**. **The only thing needed: an `OPENAI_API_KEY` with fine-tuning enabled.**
Once it's set, everything is scripted — no further decisions.
