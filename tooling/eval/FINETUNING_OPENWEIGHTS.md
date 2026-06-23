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
messages-only chat format; deterministic seeded split). Train conventions-OFF, shots-0:

```sh
$MLX_PY ../corpus/gen_corpus.py --combinatorial --out /var/tmp/claude/corpus-train.jsonl   # ~2.7k verified examples
$MLX_PY export_finetune.py --corpus /var/tmp/claude/corpus-train.jsonl \
        --conventions off --shots 0 --mlx-data /var/tmp/claude/mlxdata                      # ~5.3k SFT pairs
```

**Train/test integrity:** train on the **combinatorial** corpus, evaluate on the **curated** `corpus.jsonl`
(disjoint shapes/constants), graded by `nl-validator` — no leakage.

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

## Results — the bet, confirmed (Qwen2.5-1.5B, MLX LoRA, local, $0)

First local run, **2026-06-23**: trained conventions-OFF on the combinatorial corpus (5,028 pairs, 1200
iters), evaluated **conventions-OFF / shots-0** on the curated 304-task pool (surface-exact `pass`):

| condition | write | read | assemble | total |
|---|---|---|---|---|
| base (no adapter)     | **0/151 (0.0%)** | 43/141 (30.5%) | 12/12 (100%) | 55/304 (18.1%) |
| **+ LoRA (corpus-tuned)** | **129/151 (85.4%)** | 111/141 (78.7%) | 12/12 (100%) | **252/304 (82.9%)** |

**`write`: 0% → 85.4%, purely from SFT** — no conventions in the prompt, no few-shot. The base model
literally cannot emit Nova Lingua surface; the fine-tuned one writes it. `read` likewise jumps 30.5% →
78.7%. Decisively, **tuned surface ≈ semantic** (write 85.4% surface vs 86.1% after dialect-repair) — i.e.
training learned the *surface dialect itself*, closing the very gap that 3–10 few-shot examples never could
(the in-context conventions-off ceiling for `write` was ~26% surface / ~50% semantic). This is the
corpus-teaches-the-dialect thesis, validated on a **trainable, redistributable, Apache-2.0 open-weights
model** — and the adapter is the artifact the OSS commons can actually ship.

## Notes

- **The adapter is the shippable artifact.** Unlike a closed managed fine-tune, the LoRA adapter (and the
  recipe to reproduce it) can live in the repo / commons. Next step once the signal is confirmed: pin a base
  + adapter and treat it as the reference "speaks-Nova-Lingua" checkpoint.
- **Cost: $0.** Everything here is local. The only network traffic is the one-time base-model download from
  the HF Hub. Contrast `FINETUNING.md` (OpenAI), which bills ~$10–25 but is a clean cross-check on a
  different base family.
