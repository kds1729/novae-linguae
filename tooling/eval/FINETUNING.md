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

## 3. Base model + method (recommendation)

The task is a **narrow DSL**, not open-ended language — a small open-weight instruct model fine-tuned with
**LoRA/QLoRA** should learn the surface dialect from enough examples; a frontier model is unnecessary.
Recommended starting point: a 7–8B instruct model (e.g. Qwen2.5-7B-Instruct or Llama-3.1-8B-Instruct),
LoRA, 2–3 epochs over the SFT pairs. Scale the dataset (more `_K*` breadth, more families) before scaling
the model.

## 4. Eval protocol (mostly free; the run bills)

Reuse this harness. Add a client for the chosen provider to `model_client.py` (a thin `answer(task)` over
an OpenAI-compatible / provider endpoint — small, free to write). Then:
```sh
python3 eval_harness.py --model <fine-tuned-id> --conventions off   # the key comparison
```
**Success criterion:** fine-tuned **conventions-off** `write` should rise from the ~26%/50% in-context
baseline toward the ~99% that stated conventions buy in-context — i.e., the model has internalized the
dialect from training rather than needing the rules spelled out. Watch `write` (the gap); `read` is
already strong. The surface-vs-semantic split still applies (dialect tax vs reasoning).

---

## The money gate (needs sign-off + API key)

Two costs, both the user's call:
1. **The fine-tune run.** Managed LoRA SFT on ~5–10k short examples is typically inexpensive (rough order:
   single-digit to low-tens of dollars on Together/Fireworks/OpenAI managed fine-tuning; self-host on one
   GPU trades dollars for GPU-hours). Exact pricing to confirm at provider selection.
2. **Evaluating the fine-tuned model** through `eval_harness.py` — bills per the provider's inference
   pricing, like the in-context runs (those measured ~$1/full-pool run on Opus 4.8 at medium effort).

**Decision needed:** pick a provider/base model and a budget. Once chosen, the remaining work is: add the
provider client to `model_client.py` (free), export the dataset (free), run the fine-tune + held-out eval
(billed). Everything up to that point is done.
