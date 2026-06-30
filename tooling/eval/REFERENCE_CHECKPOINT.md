# Reference checkpoint — the "speaks-Nova-Lingua" model

This pins the current best fine-tuned models for *Nova Lingua*. Per the project finding that **the corpus
teaches the shapes and model capacity supplies the headroom to apply them**, the reference is a LoRA adapter
over a code-pretrained Qwen2.5-Coder base. There are **two reference tiers** (both on the same `corpus7`):

| tier | base | write (held-out) | notes |
|---|---|---|---|
| **best accuracy** | **Coder-7B** | **141/151 = 93.4%** (both seeds) | seed-stable; **cracks `nand`** (capacity-bound, survived all corpus work + 3B) |
| **efficient** | Coder-3B | 138/151 = 91% (best seed; ~134 mean) | half the size/VRAM; the deployable sweet spot |

7B both raised the score (+3–7 write) **and** killed the 3B seed variance (3B swung 130↔138; 7B is 141 on
both seeds) — the capacity lever, confirmed. Pick 7B when accuracy matters, 3B when size/latency does. The
detailed recipe below is the **3B efficient default**; the 7B differs only in `--base` (and weights live in
`adapter-coder7b-c7-s1`).

A LoRA adapter is small, but the *recipe* is what makes it a checkpoint: the run is **deterministic**
(fixed seed, greedy eval, no RNG in the data path), so this manifest reproduces the adapter bit-for-bit
on the same base + corpus. The weights themselves are gitignored (regenerable); pin/host them in the
commons, not the source tree.

## The pin

| | |
|---|---|
| **Base model** | `Qwen/Qwen2.5-Coder-3B-Instruct` (Apache-2.0) |
| **Method** | LoRA, r=16, α=32, dropout=0.05, targets = all attn+MLP proj |
| **Training** | 2 epochs, **seed 1**, bf16, `--max-seq-len 512`, lr 2e-4, grad-accum 8 (RTX 4090) |
| **Trainer** | [`train_lora_cpu.py`](train_lora_cpu.py) (auto-uses CUDA when present) |
| **Corpus** | `corpus7.jsonl` — 3,164 examples / 2,966 combinatorial specs, **36 template families** (incl. #35 min/max & clamp, #36 powers & digit arithmetic) (`gen_corpus.py --combinatorial`) · sha256 `1b158bfd83f9e992…` |
| **Train split** | `ftdata7/` — 5,568 train / 293 valid, **conventions-off, curated eval held out** (`export_finetune.py --holdout-corpus`) · `train.jsonl` sha256 `dc4fbf16bf961adf…` |
| **Grading** | [`eval_harness.py`](eval_harness.py) `--conventions off --shots 0`, curated set held out of training |
| **Adapter weights** | `adapter-coder3b-c7-s1` (regenerable; gitignored, stored in scratch/commons) |

## Measured result (held out, conventions-off, shots-0)

From `coder3b-c7-s1_eval.jsonl` (the 2026-06-29 corpus7 GPU run, **seed 1** — the best checkpoint):

| kind | surface-exact | semantic | n |
|---|---|---|---|
| **write** | **138 / 151 (91.4%)** | 138 / 151 | 151 |
| read | 123 / 141 (87.2%) | 129 / 141 (91.5%) | 141 |
| assemble | 12 / 12 (100%) | 12 / 12 (100%) | 12 |
| **total** | **273 / 304 (89.8%)** | 279 / 304 (91.8%) | 304 |

**Read this as a best-checkpoint, not a corpus-level jump.** Seed 0 of the same run scored write 130/151, so
the 2-seed mean (~134) is statistically flat against corpus6 (137/132, mean ~134.5) — the ±10/seed write
floor dominates the aggregate now. What corpus7 (family #36) *did* add is **two robust capability fixes**:
`square_diff` and `sum_digits` now pass **both** seeds (the model writes `mul a a` / div-mod recursion
instead of the invented `a^2` / `show`). base `write` is 0% — the adapter is the whole signal.

## Reproduce / regenerate the weights

This box has no GPU, so a 3B base is a GPU step. The portable recipe:

```bash
# 1. build the leakage-guarded SFT split (local, $0)  [corpus7 = --combinatorial regen of gen_corpus.py]
python3 tooling/eval/export_finetune.py --corpus corpus7.jsonl --conventions off --shots 0 \
    --holdout-corpus tooling/corpus/corpus.jsonl --mlx-data ftdata7

# 2. train (the SAME script runs on CPU or GPU; on GPU it auto-selects CUDA+bf16)
python3 tooling/eval/train_lora_cpu.py --train ftdata7/train.jsonl \
    --base Qwen/Qwen2.5-Coder-3B-Instruct --out adapter-coder-3b \
    --epochs 2 --seed 1 --max-seq-len 512 --dtype bfloat16

# 3. grade held-out, same settings as the pin
NL_HF_DTYPE=bfloat16 python3 tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::adapter-coder-3b --conventions off --shots 0
```

The GPU operational details (renting a box, transfer, the pod-side gotchas) are in the local-only
`RUNPOD.md` one directory up — deliberately kept out of this public repo.

## Residuals & the plateau (2-seed)

Two corpus-breadth families have now each fixed their named targets on **both** seeds:
- **#35 (min/max & clamp)** — `clamp`, `in_range`, `max_list_rec`, `max_self`, `min_of_list`, and the
  `max/min_*_absorb` laws all went `.`→`P P` in corpus6.
- **#36 (powers & digits)** — `square_diff` and `sum_digits` went `.`→`P P` in corpus7 (model now uses
  `mul a a` / div-mod recursion, not the invented `a^2` / `show`).

**But the aggregate write score has plateaued at ~134/151 (2-seed mean), ~91% best single seed.** Each
targeted family reliably fixes its 1–2 named tasks, yet the net is swamped by ~±10/seed churn — e.g.
corpus7 fixed square_diff+sum_digits but the *same noise* knocked the #35 `clamp`/absorb tasks back on one
seed. So **the corpus-breadth lever is into diminishing returns**: it still buys specific, durable
capabilities, but no longer moves the headline number above the noise.

Genuinely hard residuals that corpus breadth could NOT move turned out to be **capacity-bound, and the 7B
run confirmed it**: `nand` (survived every corpus round + 3B) and `reverse_concat` both cracked at 7B on
**both** seeds, and 7B erased the 3B seed variance entirely. So the lever for these was capacity, exactly as
hypothesized — not more families. Still failing even at 7B (a small, mostly heavily-covered-and-thus-noise
set): `nth` (still invents an `error` builtin the language lacks), `member`, `reverse`, `max_list_rec`,
`foldr_with`, `pow2` (flickers). `implies`, `concat_lists` (old 1.5B residuals) stay solved.
