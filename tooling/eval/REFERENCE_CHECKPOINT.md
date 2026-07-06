# Reference checkpoint — the "speaks-Nova-Lingua" model

This pins the current best fine-tuned models for *Nova Lingua*. Per the project finding that **the corpus
teaches the shapes and model capacity supplies the headroom to apply them**, the reference is a LoRA adapter
over a code-pretrained Qwen2.5-Coder base. The eval is the **370-task** curated set (184 write tasks —
incl. the expressiveness-phase string/map/JSON tasks and the corpus13 sort/case rows); 3B/7B are on
`corpus13`, 14B on `corpus10`:

| tier | base | write (held-out, s0 / s1) | notes |
|---|---|---|---|
| **best write** | **Coder-7B** (corpus13) | **166 / 174 of 184 (94.6% best, 92.4% mean)** | new best-ever write rate; the 7B `reverse` regression (3 tasks) swept by family #44; 4/5 of the new sort/case tasks on first contact |
| **best total / read** | Coder-14B (corpus10) | 156 / 162 of 179 (88.8% mean) | total semantic **92.2–93.6%**, read semantic **96.4–97.0%** on the 360-task set — capacity owns *reading*, on pattern; three corpora behind (not retrained on #41–#44) |
| **efficient** | Coder-3B (corpus13) | 166 / 172 of 184 (93.5% best, 91.8% mean) | best 3B ever — seed-1 beats corpus11's 164/179 rate and passes the whole old residual core (`modulo`/`pow2`/`is_int_string`) that seed |

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

> **The corpus10→11 double crank (2026-07-04 evening) — the loop closed twice in one day.** The
> expressiveness phases added 22 new write tasks (strings/maps/JSON; eval 316→360 tasks incl. 6
> few-shot-reserved at shots-3). **corpus10** (families #39/#40, composite idioms) taught 14–15/22 on
> first contact — and the per-task diagnosis of the rest was exact: the consistent failures were
> builtins that **never appeared in training at all** (composite idioms only; bare golds
> leakage-drop) — the model recursed over a *string* as if it were a list and invented
> `keys`/`sort`/regex. **corpus11** (family #41, near-bare builtin usage) swept them: `str_len`,
> `key_list`(3B), `drop_key`, `store_one`, `is_valid_json`, `canonical_json` all flipped `.`→`P P`,
> and the `reverse` regression corpus10 induced on all three tiers (dilution churn — a wrong cons
> order) fully recovered. Net: 7B write 159→**167** and 3B 157→**164 best**, with corpus11's 3B
> beating corpus10's 7B. The lesson to keep: **a corpus teaches operations only if they appear in
> some training shape — composite idioms don't transfer down to the bare builtin.** Remaining 7B
> both-seed residuals (7): `take_rec`/`drop_rec` (the designed index-walk family),
> `divide`-adjacent `modulo`, and per-seed churn (`key_list`, `max_of_list`/`min_of_list`,
> `product`).

> **The corpus12 round (2026-07-05, 3B/7B 2-seed) — family #42 swept its targets.** The last
> *designed-but-unbuilt* coverage gap closed: `write/take_rec` and `write/drop_rec` (the list-returning
> index walks, residual since corpus8) flipped `.`→`P` at **both** tiers, plus `read/take_rec` at 3B and
> `read/drop_rec` at 7B. Headline write is flat (7B 167 = corpus11's 167; 3B 163 vs 164) — the corpus
> absorbed a new family without paying for it, and the designed residuals are now churn-or-better.
> One watch item: `write/reverse` regressed at 7B on **both seeds** (the same wrong-cons-order dilution
> symptom corpus10 induced and #41's near-bare shapes fixed; 3B holds it) — if it persists into the next
> round, the #41 lever (more bare `reverse`-adjacent shapes) is the known fix. Post-#42 residual write
> core: `modulo` (flaky at every scale since 1.5B), `pow2` (the documented generalization limit), the 3B
> bare parse-predicate shape — everything else is per-seed churn.

> **The corpus13 round (2026-07-06, 3B/7B 2-seed, RTX 4090) — the GW4 pull gets its training shapes,
> and the reverse watch item closes.** Family #43 (sort/case: near-bare `str_lt`/`str_lower`, the
> min-vs-constant case-select, order filters, transformed insert-into-sorted walks) taught **4 of the
> 5 new curated write tasks on first contact at every tier/seed** (`sorts_before`, `min_string`,
> `ci_equal`; `lowercase` 3-of-4); family #44 (bare-reverse reinforcement, the #41 lever re-applied)
> **swept the 7B reverse regression on both seeds** — `write/reverse`, `write/reverse_concat`, and
> `write/reverse_naive_cost` all flipped F→P vs the c12-s1 pin. Headline: **7B-s1 write 174/184 =
> 94.6%, the best write rate ever** (line-comparable old-set subset 169/178 vs c12's 166), and **3B-s1
> 172/184 = 93.5%, the best 3B ever** (old-set 167/178 vs c12's 162) — that seed even passes `modulo`,
> `pow2`, and `is_int_string` together. The one consistent new miss: **`write/insert_sorted`** (the
> full insert-into-sorted recursion, 0/4 cycles; `read/insert_sorted` 1/4) — the new named residual,
> with `read/min_string` (2/4) its churny read-side companion. `pow2` passed all four cycles for the
> first time.

Pick 7B when accuracy matters, 3B when size/latency does; 14B only when *read* accuracy is the point. The
detailed recipe below is the **3B efficient default**; the 7B differs only in `--base` (weights
`adapter-coder7b-c13-s1`, sha256 `3d4625ff5111504b…`, seed 1; the 14B read-champion weights are
`adapter-coder14b-c10-s1`, sha256 `502a67715c0909df…`).

A LoRA adapter is small, but the *recipe* is what makes it a checkpoint: the run is **deterministic**
(fixed seed, greedy eval, no RNG in the data path), so this manifest reproduces the adapter bit-for-bit
on the same base + corpus. The weights themselves are gitignored (regenerable) and **hosted in the
commons** per [`spec/weights.md`](../../spec/weights.md): all three pinned tiers are published to Arca
as `wgt_` pointer records with signed eval attestations of the measured scores, blobs fetchable (and
hash-verifiable) from `https://nl.1105software.com/v0/blobs/<sha256>` —
3B `wgt_6f61b11fc448f51f…`, **7B `wgt_eb75d914b03ab2d1…`**, 14B `wgt_19cb0740b13c354e…`
(records + attestations committed under [`spec/examples/`](../../spec/examples/); the c13 records
carry `supersedes` links to the corpus12 pins `wgt_fd2b1c74…`/`wgt_3a9906ac…`, which stay resolvable
— the commons is append-only, so a re-pin is a new record, not an overwrite).

## The pin

| | |
|---|---|
| **Base model** | `Qwen/Qwen2.5-Coder-3B-Instruct` (Apache-2.0) |
| **Method** | LoRA, r=16, α=32, dropout=0.05, targets = all attn+MLP proj |
| **Training** | 2 epochs, **seed 1**, bf16, `--max-seq-len 512`, lr 2e-4, grad-accum 8 (RTX 4090) |
| **Trainer** | [`train_lora_cpu.py`](train_lora_cpu.py) (auto-uses CUDA when present) |
| **Corpus** | `corpus13.jsonl` — 3,354 examples / 3,123 combinatorial specs, **44 template families** (incl. #41 near-bare builtins, #42 list-returning index walks, #43 sort/case, #44 bare-reverse reinforcement) (`gen_corpus.py --combinatorial`) · sha256 `c967db616e37a48e…` |
| **Train split** | `ftdata_c13/` — 5,855 train / 308 valid, **conventions-off, curated eval held out** (`export_finetune.py --holdout-corpus`; 417 eval-matched tasks excluded) · `train.jsonl` sha256 `05868f3f402572c4…` |
| **Grading** | [`eval_harness.py`](eval_harness.py) `--conventions off --shots 0`, curated set held out of training |
| **Adapter weights** | `adapter-coder3b-c13-s1` (regenerable; gitignored). Local copy: `/var/tmp/claude/round13/adapter-coder3b-c13-s1/adapter_model.safetensors`, **sha256 `4659cb43ad3059dae4b2cf4390e9a41c43cc2905860fafb3fa2714b7ca3c1f6c`** (LoRA r16/α32/dropout0.05, targets = all attn+MLP proj — matches this pin) |

## Measured result (held out, conventions-off, shots-0)

From `coder3b-c13-s1_eval.jsonl` (the 2026-07-06 corpus13 GPU run, **seed 1** — the best 3B checkpoint,
on the 370-task eval that includes the expressiveness-phase string/map/JSON tasks and the sort/case rows):

| kind | surface-exact | semantic | n |
|---|---|---|---|
| **write** | **172 / 184 (93.5%)** | 172 / 184 | 184 |
| read | 149 / 174 (85.6%) | 155 / 174 (89.1%) | 174 |
| assemble | 12 / 12 (100%) | 12 / 12 (100%) | 12 |
| **total** | **333 / 370 (90.0%)** | 339 / 370 (91.6%) | 370 |

Seed 0 of the same run scored write 166/184 (the 2-seed mean is **91.8%** — the best 2-seed 3B mean yet).
The 7B tier (same recipe, `--base Qwen/Qwen2.5-Coder-7B-Instruct`, weights `adapter-coder7b-c13-s1`) is
**174/166 of 184 (94.6% best)** — the number to quote for the project's best write. base `write` is 0% —
the adapter is the whole signal.

## Reproduce / regenerate the weights

This box has no GPU, so a 3B base is a GPU step. The portable recipe:

```bash
# 1. build the leakage-guarded SFT split (local, $0)  [corpus13 = --combinatorial regen of gen_corpus.py]
python3 tooling/eval/export_finetune.py --corpus corpus13.jsonl --conventions off --shots 0 \
    --holdout-corpus tooling/corpus/corpus.jsonl --mlx-data ftdata13

# 2. train (the SAME script runs on CPU or GPU; on GPU it auto-selects CUDA+bf16)
python3 tooling/eval/train_lora_cpu.py --train ftdata13/train.jsonl \
    --base Qwen/Qwen2.5-Coder-3B-Instruct --out adapter-coder-3b \
    --epochs 2 --seed 1 --max-seq-len 512 --dtype bfloat16

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
m = HFModel("Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/round13/adapter-coder3b-c13-s1")  # base::adapter
task = SimpleNamespace(system="You write Nova Lingua function records.",
                       user="Write a function record for: double a natural number.")
print(m.answer(task))
```

Or drive it straight through the harness (loads the adapter, prompts, and grades every answer with
`nl-validator` — the trustworthy way to *use it and see it's right* at once):

```bash
NL_HF_DTYPE=bfloat16 python3 tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/round13/adapter-coder3b-c13-s1 \
    --conventions off --shots 0            # add --tasks write --limit N for a quick subset
```

The `hf:<base>::<adapter>` spec routes to `HFModel`; `mlx:<base>::<adapter>` routes to Apple MLX
(`MLXModel`). Base model and adapter both download/load from the HF cache or a local path. **base `write`
is 0%** — the adapter is the entire signal, so it must be present.

## Verify the pinned number (GPU)

The `write 172/184` figure was measured once on the training pod (now terminated). The local weights
above reproduce it, but a full 370-task CPU eval of a 3B model is multi-hour — run the *verification* on a
rented GPU instead (see the local-only `RUNPOD.md`), where a full held-out eval of the existing adapter is
~5 min. On a fresh pod (base cache warmed, repo + `adapter-coder3b-c13-s1` + `ftdata13` uploaded to `/root`;
NB the repo clone needs the corpus13 `tooling/corpus/corpus.jsonl` — the eval pool):

```bash
# re-evaluate the EXISTING pinned adapter (no retrain) against the held-out curated set
NL_HF_DTYPE=bfloat16 python -u repo/tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/root/adapter-coder3b-c13-s1 \
    --conventions off --shots 0 --out /root/verify_c13_s1_eval.jsonl
```

Expect `write` ≈ 172/184 (seed-1 pin). A full retrain-then-eval (confirms the *recipe*, not just the
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

**The residual write core after corpus13** (everything else is per-seed churn): the corpus12 watch
item is closed — family #44 swept the 7B reverse regression (`reverse`/`reverse_concat`/
`reverse_naive_cost`, F→P both seeds) — and `pow2` passed all four corpus13 cycles for the first
time. What remains: **`insert_sorted`** (the new named residual — the full insert-into-sorted
recursion, `write` 0/4 cycles; the near-bare #43 shapes taught the *operations* but not yet the
whole two-case recursion — the transformed insert walks are in training, so this may be the
`foldr_with` pattern where the idiom needs another round or capacity), `modulo` (the
division-arithmetic corner, flaky at every scale since 1.5B — passed 3B-s1 this round), and the
bare parse-predicate shape (`is_int_string` — passed 3B-s1, failed 7B both seeds this round).
`implies`, `concat_lists`, `nand` (older residuals) stay solved.

> **Eval-set lineage note.** The expressiveness phases (2026-07-04) grew the curated eval 316 →
> **360 graded tasks** (179 write / 169 read / 12 assemble), and the corpus13 sort/case rows
> (2026-07-06) grew it 360 → **370 graded tasks at the shots-0 setting (184 write / 174 read /
> 12 assemble)** — 5 new curated functions (`sorts_before`/`min_string`/`lowercase`/`ci_equal`/
> `insert_sorted`), each a write + a read task; the oracle re-grades 370/370. The corpus8-era
> numbers in the history above are on the **316-task** set and the corpus10–12 numbers on the
> **360-task** set — not line-comparable to the current tier table without the old-set subset
> figures quoted alongside (c13-s1 holds 169/178 at 7B and 167/178 at 3B on the shared old-set
> write tasks, both above their c12 pins). Historical write ceilings for line comparison:
> corpus8 7B = 150/157 on the old set; the aggregate is now carried by a strictly larger,
> harder task pool. The GW5 float-report pull (2026-07-06, later the same day) grew the eval
> again 370 → **380 (189 write / 179 read / 12 assemble**; oracle 380/380) with the float rows
> `to_float`/`show_float`/`half_of`/`mean_of`/`stat_line`; the corpus14 retrain (family #45 +
> the 14B re-baseline, driver staged) re-measures the tiers on that set.
