# Fine-tuning an OPEN-WEIGHTS model to speak Nova Lingua — local, free, self-hostable

This is the open-weights arm of the fine-tune experiment, and the one that actually fits the project's
ethos: a closed `ft:gpt-4o-mini` checkpoint can't ship in a content-addressed OSS commons, but an
**open-weights LoRA adapter** can. It also costs **nothing** — the whole cycle (base eval → LoRA train →
tuned eval) runs **locally on Apple Silicon via [MLX](https://github.com/ml-explore/mlx)**, no API, no key.
See `FINETUNING.md` for the OpenAI managed-SFT path; the two share the same corpus and exporter, so the
decision between them is purely about where the trained weights live.

The bet is unchanged: in-context few-shot has hit its ceiling as a proxy (conventions-*off* `write` is the
weak spot), so the corpus must be **trained on**, not shown in-context. This runbook trains
**conventions-OFF** and evaluates **conventions-OFF, shots-0** — a clean test of whether SFT *internalized*
the dialect, with nothing in the prompt to lean on.

## Why these choices

- **Local MLX, not a rented GPU or managed host** — it's free, it's reproducible on the dev machine, and
  the artifact (a LoRA adapter) is something you can host and redistribute. Managed open-weights SFT
  (Together/Fireworks) or a rented GPU are drop-in alternatives if you outgrow the laptop; the exported
  data format is portable.
- **Qwen2.5 base** — **Apache-2.0 licensed**, matching the project's own dual Apache-2.0/MIT licensing, so
  a fine-tune can ship in the commons with no license friction. (Llama's community license is murkier for
  OSS redistribution.) Nova Lingua is a narrow DSL, not open-ended language, so a small base (1.5B–7B) plus
  enough verified examples should learn the surface dialect; the runbook starts at **1.5B** for fast
  iteration and makes the base a knob.

## 0. One-time stack (free)

MLX ships **arm64-only** wheels and needs a **native arm64 Python** (an Intel/Rosetta Python yields
`macosx-…-x86_64` tags and `pip` finds no MLX wheel). `uv` fetches a standalone arm64 CPython:

```sh
curl -LsSf https://astral.sh/uv/install.sh | sh
uv python install 3.12
uv venv /var/tmp/claude/mlx-venv2 --python 3.12
uv pip install --python /var/tmp/claude/mlx-venv2/bin/python mlx-lm
```

`MLX_PY=/var/tmp/claude/mlx-venv2/bin/python` below.

## 1. Data (free) — same corpus + exporter as the OpenAI path

`export_finetune.py --mlx-data DIR` writes an MLX `--data` directory (`train.jsonl` + `valid.jsonl`,
messages-only chat format; deterministic seeded split). Train conventions-OFF, shots-0.

**Train/test integrity — this is load-bearing, don't skip it.** `gen_corpus.py --combinatorial` emits the
curated corpus *plus* the combinatorial specs — it is a **SUPERSET of the curated eval set**. Train on it
naively and the curated eval is **100% leaked** (every eval prompt+gold seen verbatim), and the score
measures memorization, not generalization. **Always pass `--holdout-corpus <curated corpus.jsonl>`** so the
exporter drops every training task whose `(prompt, gold)` matches an eval task:

```sh
CURATED=../corpus/corpus.jsonl
$MLX_PY ../corpus/gen_corpus.py --combinatorial --out /var/tmp/claude/corpus-train.jsonl   # ~2.7k verified examples
$MLX_PY export_finetune.py --corpus /var/tmp/claude/corpus-train.jsonl --holdout-corpus "$CURATED" \
        --conventions off --shots 0 --mlx-data /var/tmp/claude/mlxdata                      # ~5k SFT pairs (304 leaked tasks dropped)
```

Even with the exact eval tasks removed, the combinatorial set still contains **parametric twins** of many
curated shapes (same template, different constants), so the held-out curated score is closer to a
*generalize-across-constants* test than a *generalize-to-unseen-shapes* test — read the curated number with
that ceiling in mind. Curated shapes with no combinatorial twin (variant-match, nested HOF, multi-clause
case, Locutio) are the genuinely novel held-out tasks.

## 2. Train (free, local) — MLX LoRA

```sh
$MLX_PY -m mlx_lm lora \
  --model mlx-community/Qwen2.5-1.5B-Instruct-bf16 \
  --train --data /var/tmp/claude/mlxdata \
  --fine-tune-type lora --num-layers 16 \
  --batch-size 8 --iters 1200 --max-seq-length 2048 \
  --learning-rate 1e-4 --mask-prompt \
  --adapter-path /var/tmp/claude/nl-adapter \
  --steps-per-report 50 --steps-per-eval 300 --save-every 400
```

Produces a LoRA adapter at `--adapter-path` (~21 MB of safetensors + config; 5.28M trainable params,
0.34% of the model). `--mask-prompt` trains the loss on the assistant completion only (generate-the-body,
not reproduce-the-prompt). On an M4 Pro this is ~0.7 it/s, ~30 min for 1200 iters (≈2 epochs), peak ~10 GB.
`--num-layers` / `--iters` / `--learning-rate` are the knobs; scale the base (3B/7B) if the signal is weak.

## 3. Evaluate (free, local) — base vs. tuned, conventions-OFF

The harness routes `mlx:<repo>` to a local on-device run (no API, no cost). `mlx:<repo>::<adapter_dir>`
loads the LoRA adapter. The clean internalization test is **conventions off, shots 0** — nothing in the
prompt teaches the dialect, so any lift is from the weights:

```sh
# baseline — base model, no adapter
$MLX_PY eval_harness.py --model "mlx:mlx-community/Qwen2.5-1.5B-Instruct-bf16" \
        --conventions off --shots 0 --out /var/tmp/claude/base_eval.jsonl

# headline — same model + the corpus-trained LoRA adapter
$MLX_PY eval_harness.py --model "mlx:mlx-community/Qwen2.5-1.5B-Instruct-bf16::/var/tmp/claude/nl-adapter" \
        --conventions off --shots 0 --out /var/tmp/claude/tuned_eval.jsonl
```

**Success criterion:** the tuned model's conventions-off `write` should rise sharply from the base floor
toward what *stated conventions* buy a capable model in-context — i.e., training internalized the dialect.
Watch `write` (the gap); `read` is already comparatively strong. The surface-vs-semantic split still
applies (a tuned model that learns the dialect should close the gap from the *surface* side).

## Results (Qwen2.5-1.5B, MLX LoRA, local, $0) — **2026-06-23**, conventions-OFF / shots-0, curated 304-task pool

| condition | write | read | assemble | total |
|---|---|---|---|---|
| base (no adapter)                | 0/151 (0.0%)   | 43/141 (30.5%) | 12/12 (100%) | 55/304 (18.1%) |
| tuned, **LEAKED** (curated in training) | 129/151 (85.4%) | 111/141 (78.7%) | 12/12 (100%) | 252/304 (82.9%) |
| tuned, **held-out** (curated removed via `--holdout-corpus`) | **37/151 (24.5%)** | **51/141 (36.2%)** | 0/12 (0%) | **88/304 (28.9%)** |

**The held-out row is the real result; the leaked row is a cautionary contrast.** A first run trained on the
naive combinatorial corpus and scored 82.9% — but that corpus is a superset of the eval set, so the model
had memorized every eval task; the number measured recall, not learning. With the curated eval set held out
of training, the honest generalization lift is **write 0% → 24.5%, read 30.5% → 36.2%**.

What that means, read straight:
- **The dialect IS partially learnable from SFT.** Base `write` is a hard 0% (the model cannot emit Nova
  Lingua surface at all), so 24.5% on genuinely held-out tasks is real learning, not lookup.
- **But 1.5B + ~5k pairs is far from "speaks the language."** 24.5% is well below both the memorized number
  and the in-context conventions-*on* ceiling (~99%), and it's likely an *upper* bound on novel-shape
  generalization because parametric twins remain in training (see the integrity note above).
- **`read` barely moves (+5.7pts)** — comprehension was already the stronger skill and gains little here.
- **`assemble` regresses 100% → 0%** — the held-out training had *zero* assemble examples (all 12 curated
  ones were held out, and the combinatorial generator emits none), so the adapter shifted the model out of
  the assemble format (catastrophic interference). Fix: include assemble shapes in training data.

**Honest bottom line:** open-weights SFT teaches the surface dialect *partially* and cheaply, but this run
does **not** establish "a model that speaks Nova Lingua." The infra and the leakage guard are the durable
deliverables; the adapter is a redistributable artifact, but not yet a strong one.

### Scale + diagnosis: the bottleneck is data diversity, not model size

Same held-out data, same hyperparameters, larger base:

| base | write | read | assemble | total |
|---|---|---|---|---|
| Qwen2.5-1.5B | 24.5% | 36.2% | 0%    | 28.9% |
| Qwen2.5-3B   | **31.8%** | 44.7% | 41.7% | **38.2%** |

Doubling the base buys only **+7-9 pts** on write/total (3B also partly recovered `assemble`, 0→42%, being
more robust to the no-assemble-in-training interference). Far short of the conventions-*on* in-context
ceiling (~99%), so capacity is not the main lever. The **per-family** 3B write breakdown shows why:

| curated write family | 3B pass | has a parametric twin in training? |
|---|---|---|
| simple arithmetic   | 45% | yes (same shape, other constants) |
| comparisons / bool  | 56% | yes |
| list / higher-order | **5%**  | structurally novel |
| recursion           | **0%**  | structurally novel |
| variant / case-match| 33% | structurally novel |
| other               | **9%**  | structurally novel |

Families the combinatorial generator covers (varying constants of a fixed shape) generalize at 45-56%; the
structurally novel families it does **not** cover generalize at 0-9%. The model learned to apply the dialect
across *constant variations of seen shapes*, not to *unseen structural shapes*. **So the real lever is
broadening the combinatorial generator's structural coverage — more distinct shapes (HOF compositions,
recursion templates, variant-matching, multi-clause case), not just more constants — and keeping `assemble`
in the data.** A 7B base would likely add a few more points but not close the structural gap.

## Notes

- **The adapter is the shippable artifact.** Unlike a closed managed fine-tune, the LoRA adapter (and the
  recipe to reproduce it) can live in the repo / commons. Next step once the signal is confirmed: pin a base
  + adapter and treat it as the reference "speaks-Nova-Lingua" checkpoint.
- **Cost: $0.** Everything here is local. The only network traffic is the one-time base-model download from
  the HF Hub. Contrast `FINETUNING.md` (OpenAI), which bills ~$10–25 but is a clean cross-check on a
  different base family.
