# Reference checkpoint — the "speaks-Nova-Lingua" model

This pins the current best fine-tuned models for *Nova Lingua*. Per the project finding that **the corpus
teaches the shapes and model capacity supplies the headroom to apply them**, the reference is a LoRA adapter
over a code-pretrained Qwen2.5-Coder base. All tiers are on `corpus8` (2-seed, 157 write tasks):

| tier | base | write (held-out, s0 / s1) | notes |
|---|---|---|---|
| **best write** | **Coder-7B** | **149 / 150 (95.5% best, 95.2% mean)** | near-seed-stable; on corpus8 it **matches-or-beats 14B on write** at half the size — `foldr_with`/`member` (once "capacity-only") crack at 7B given the idiom families |
| **best total / read** | Coder-14B | 144 / 150 (93.6% mean) | total semantic **95.3–95.9%**, read semantic **95.9–98.6%** — capacity still owns *reading*; but c8 exposed 14B write seed-variance (±6) that 7B doesn't have |
| **efficient** | Coder-3B | 143 / 140 (90.1% mean) | half the 7B size/VRAM; corpus8 lifted the mean +1.4 pts and halved the seed swing (8→3 tasks) |

> **The capacity boundary (2026-07-03, Coder-14B 2-seed).** 14B moved the total to **96.2%** but taught the sharpest lesson: **capacity fixes *reading*, not *writing*.** `read` climbed 91→98.6% (the off-by-one / sign / absorption-law arithmetic errors are capacity-bound and mostly gone), and the two genuine reasoning-*write* residuals `foldr_with`/`member` cracked on both seeds — yet the `write` count is **dead-stable at 147/157 across both seeds AND across 7B↔14B**. The remaining write misses are not capacity-bound: they are the dialect's **totality by design** (no `^`, no `!!`, no `error`). Two of them were a missing-*idiom* gap, closed at $0/local: adding **`last`/`init`** list builtins made the model's already-correct `reverse` valid, and **corpus family #38** (index recursion) flipped `nth` `.`→`P` at 14B (`min_of_list` too, via #37). Genuinely stuck: `pow2` (its exact gold is the already-covered `rec_pow` shape → leakage-dropped → a generalization limit, not a coverage gap) and a small arithmetic core (`fib`, sign). Adapters (275 MB each) pulled to `/var/tmp/claude/adapter-coder14b-c{7,8}-s*`.

> **The corpus8 re-pin (2026-07-04, all tiers 2-seed).** Retraining every tier on corpus8 (families
> #37/#38 + the `last`/`init` builtins) *revised the capacity-boundary reading*: `foldr_with`/`member`,
> which corpus7 could not move at 7B and which therefore looked capacity-bound, **crack at 7B on corpus8**
> — they were idiom-bound after all; 14B just had the headroom to guess the idiom unaided. Net effect:
> **7B write 149/150 (95.5% best seed) is the new best write tier**, beating 14B-corpus7's 147 ceiling and
> matching 14B-corpus8's mean at half the size; 3B's 2-seed mean rose ~1.4 pts with the seed swing halved.
> The residual write core, **common across all three tiers**: `divide`/`modulo` (the division-arithmetic
> corner), `pow2` (the documented generalization limit), and `take_rec`/`drop_rec` — the *list-returning*
> index walks, the one still-actionable coverage gap (#38 taught the element-returning walk `nth`; a
> family #39 would target take/drop). Everything else failing at 7B/14B is per-seed churn.

Pick 7B when accuracy matters, 3B when size/latency does; 14B only when *read* accuracy is the point. The
detailed recipe below is the **3B efficient default**; the 7B differs only in `--base` (and weights live in
`adapter-coder7b-c8-s1`, sha256 `25d720583d23fa91…`, 161,533,192 bytes).

A LoRA adapter is small, but the *recipe* is what makes it a checkpoint: the run is **deterministic**
(fixed seed, greedy eval, no RNG in the data path), so this manifest reproduces the adapter bit-for-bit
on the same base + corpus. The weights themselves are gitignored (regenerable); pin/host them in the
commons, not the source tree.

## The pin

| | |
|---|---|
| **Base model** | `Qwen/Qwen2.5-Coder-3B-Instruct` (Apache-2.0) |
| **Method** | LoRA, r=16, α=32, dropout=0.05, targets = all attn+MLP proj |
| **Training** | 2 epochs, **seed 0**, bf16, `--max-seq-len 512`, lr 2e-4, grad-accum 8 (RTX PRO 6000) |
| **Trainer** | [`train_lora_cpu.py`](train_lora_cpu.py) (auto-uses CUDA when present) |
| **Corpus** | `corpus8.jsonl` — 3,177 examples / 2,973 combinatorial specs, **38 template families** (incl. #37 single-element-base reduce, #38 index recursion) (`gen_corpus.py --combinatorial`) · sha256 `49029e028fee70ab…` |
| **Train split** | `ftdata8/` — 5,580 train / 293 valid, **conventions-off, curated eval held out** (`export_finetune.py --holdout-corpus`) · `train.jsonl` sha256 `048e543620272771…` |
| **Grading** | [`eval_harness.py`](eval_harness.py) `--conventions off --shots 0`, curated set held out of training |
| **Adapter weights** | `adapter-coder3b-c8-s0` (regenerable; gitignored). Local copy: `/var/tmp/claude/adapter-coder3b-c8-s0/adapter_model.safetensors`, 119,801,528 bytes, **sha256 `55568f944564c256803156bdbb8b13e27d28842b58e0ecbb50f37f2e764b9d0e`** (LoRA r16/α32/dropout0.05, targets = all attn+MLP proj — matches this pin) |

## Measured result (held out, conventions-off, shots-0)

From `coder3b-c8-s0_eval.jsonl` (the 2026-07-04 corpus8 GPU run, **seed 0** — the best 3B checkpoint):

| kind | surface-exact | semantic | n |
|---|---|---|---|
| **write** | **143 / 157 (91.1%)** | 143 / 157 | 157 |
| read | 132 / 147 (89.8%) | 139 / 147 (94.6%) | 147 |
| assemble | 12 / 12 (100%) | 12 / 12 (100%) | 12 |
| **total** | **287 / 316 (90.8%)** | 294 / 316 (93.0%) | 316 |

Seed 1 of the same run scored write 140/157, so the 3B 2-seed mean is **90.1%** — up from corpus7's ~88.7%
with the seed swing halved (8→3 tasks): corpus8 moved the *distribution*, not just the best draw. The 7B
tier (same recipe, `--base Qwen/Qwen2.5-Coder-7B-Instruct`, weights `adapter-coder7b-c8-s1`) is
**149/150 of 157** — the number to quote for the project's best write. base `write` is 0% — the adapter is
the whole signal.

## Reproduce / regenerate the weights

This box has no GPU, so a 3B base is a GPU step. The portable recipe:

```bash
# 1. build the leakage-guarded SFT split (local, $0)  [corpus8 = --combinatorial regen of gen_corpus.py]
python3 tooling/eval/export_finetune.py --corpus corpus8.jsonl --conventions off --shots 0 \
    --holdout-corpus tooling/corpus/corpus.jsonl --mlx-data ftdata8

# 2. train (the SAME script runs on CPU or GPU; on GPU it auto-selects CUDA+bf16)
python3 tooling/eval/train_lora_cpu.py --train ftdata8/train.jsonl \
    --base Qwen/Qwen2.5-Coder-3B-Instruct --out adapter-coder-3b \
    --epochs 2 --seed 0 --max-seq-len 512 --dtype bfloat16

# 3. grade held-out, same settings as the pin
NL_HF_DTYPE=bfloat16 python3 tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::adapter-coder-3b --conventions off --shots 0
```

The GPU operational details (renting a box, transfer, the pod-side gotchas) are in the local-only
`RUNPOD.md` one directory up — deliberately kept out of this public repo.

## Using the checkpoint (inference)

The checkpoint is a base model + a LoRA adapter. The project's own `model_client.HFModel` loads the
pair and generates (greedy, deterministic) — the same class the eval harness uses, so "using" and
"grading" go through one tested code path. Set `NL_HF_DTYPE=bfloat16` so a 3B/7B base fits in ~15 GB.

```python
# from tooling/eval/ ; `answer(task)` takes an object with .system and .user (greedy decode)
from types import SimpleNamespace
from model_client import HFModel
m = HFModel("Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/adapter-coder3b-c8-s0")  # base::adapter
task = SimpleNamespace(system="You write Nova Lingua function records.",
                       user="Write a function record for: double a natural number.")
print(m.answer(task))
```

Or drive it straight through the harness (loads the adapter, prompts, and grades every answer with
`nl-validator` — the trustworthy way to *use it and see it's right* at once):

```bash
NL_HF_DTYPE=bfloat16 python3 tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/adapter-coder3b-c8-s0 \
    --conventions off --shots 0            # add --tasks write --limit N for a quick subset
```

The `hf:<base>::<adapter>` spec routes to `HFModel`; `mlx:<base>::<adapter>` routes to Apple MLX
(`MLXModel`). Base model and adapter both download/load from the HF cache or a local path. **base `write`
is 0%** — the adapter is the entire signal, so it must be present.

## Verify the pinned number (GPU)

The `write 143/157` figure was measured once on the training pod (now terminated). The local weights
above reproduce it, but a full 316-task CPU eval of a 3B model is multi-hour — run the *verification* on a
rented GPU instead (see the local-only `RUNPOD.md`), where a full held-out eval of the existing adapter is
~5 min. On a fresh pod (base cache warmed, repo + `adapter-coder3b-c8-s0` + `ftdata8` uploaded to `/root`):

```bash
# re-evaluate the EXISTING pinned adapter (no retrain) against the held-out curated set
NL_HF_DTYPE=bfloat16 python -u repo/tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/root/adapter-coder3b-c8-s0 \
    --conventions off --shots 0 --out /root/verify_c8_s0_eval.jsonl
```

Expect `write` ≈ 143/157 (seed-0 pin). A full retrain-then-eval (confirms the *recipe*, not just the
weights) is the three-step block above run on the pod; `train_lora_cpu.py` auto-selects CUDA+bf16 there.

## Residuals & the plateau (2-seed)

The corpus-breadth families keep fixing their named targets:
- **#35 (min/max & clamp)** — `clamp`, `in_range`, `max_list_rec`, `max_self`, `min_of_list`, and the
  `max/min_*_absorb` laws all went `.`→`P P` in corpus6.
- **#36 (powers & digits)** — `square_diff` and `sum_digits` went `.`→`P P` in corpus7 (model now uses
  `mul a a` / div-mod recursion, not the invented `a^2` / `show`).
- **#37/#38 (total idioms: single-element-base reduce + index recursion)** — `nth`, `min_of_list`,
  `max_list_rec` flipped in corpus8, and (with the `last`/`init` builtins) `reverse` is valid at every
  tier. Beyond the named targets, corpus8 also cracked `foldr_with`/`member` **at 7B** — tasks corpus7
  had left looking capacity-bound.

The corpus7-era "plateau" reading (targeted fixes real, headline swamped by ±10/seed churn) softened with
corpus8: at 3B the 2-seed mean moved +1.4 pts with the swing *halved*, and at 7B the gain (+8–9 write over
corpus7) cleared the noise outright. The refined division of labor: **corpus breadth teaches idioms the
dialect requires** (totality shapes a code-pretrained model won't guess below 14B), **capacity supplies
read-side arithmetic** — and where corpus7 made residuals look capacity-bound (`foldr_with`/`member`,
cracked at 14B only), corpus8 showed the cheaper lever was the missing idiom all along.

**The residual write core after corpus8, common to all three tiers** (everything else failing at 7B/14B is
per-seed churn): `divide`/`modulo` (the division-arithmetic corner, flaky at every scale since 1.5B),
`pow2` (gold = the already-covered `rec_pow` shape → leakage-dropped → a generalization limit, not
coverage), and `take_rec`/`drop_rec` — the **list-returning index walks**, the one still-actionable
coverage gap: #38 taught the element-returning walk (`nth`), so a family of index recursions that
cons a result list (take/drop/`nth`-with-default variants) is the designed next move if the loop
continues (families **#39/#40** became the expressiveness follow-through: strings, maps & JSON — see
below). `implies`, `concat_lists`, `nand`, `reverse_concat` (older residuals) stay solved.

> **Eval-set growth note (2026-07-04, expressiveness phases 1–3).** The string builtins added 13
> curated records (+ combinatorial family #39) and the map/JSON builtins added 9 more, growing the
> curated eval to **354 tasks (176 write / 166 read / 12 assemble)** from the 316 this re-pin was
> measured on. The pinned numbers above are on the **316-task** set — not line-comparable to a future
> eval on repo HEAD; the next GPU run re-baselines on corpus10 (3,038 specs — combinatorial families
> #39 strings and #40 maps/JSON now teach all the new idioms; split at `/var/tmp/claude/ftdata10`,
> 5,697 train). Oracle stays 100% on the grown set.
