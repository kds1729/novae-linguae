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
recursion templates, variant-matching, multi-clause case), not just more constants.** A 7B base would likely
add a few more points but not close the structural gap.

### Acting on the diagnosis: broaden the generator (v2) — it works

`combinatorial_specs()` gained **+141 structurally-distinct templates** (sections 13-18), each lifted from a
hand-authored family that already passes the verify gate, then parameterized: variant-consuming
(`unwrap_or`/`map_maybe`/`unwrap_result`), search recursion (`contains`/`count_eq`), accumulating recursion
(`rec_length`/`rec_sum`), numeric recursion (`rec_times`/`rec_pow`/`rec_sumto`), nested first-order
compositions (map∘map, fold∘filter, fold∘map, count-in-range), and multi-clause `threshold3`. All verify,
zero drops; the default curated corpus stays byte-identical. The leakage guard was also tightened to drop
gold-*body* twins for write/assemble (a different prompt with the same answer body), making the v2 training
data provably free of curated-eval answers.

Same 3B base, same hyperparameters, retrained on the clean+broadened data:

| 3B training data | write | read | assemble | total |
|---|---|---|---|---|
| narrow (sections 1-12)        | 31.8% | 44.7% | 41.7% | 38.2% |
| **+ broadened (sections 1-18)** | **41.1%** | 47.5% | 58.3% | **44.7%** |

Held-out `write` **31.8% → 41.1% (+9.3 pts)** on leak-free data — the diagnosis holds: structural coverage
is the lever. The per-family write breakdown shows the gains where the new shapes landed (arithmetic 45→56%,
comparisons 56→70%, recursion 0→14%, other 9→15%); the genuinely *higher-order* families (`list/hof`,
`variant/case` — which pass function-valued arguments the templates don't yet parameterize) stayed flat. So
the next coverage frontier is function-argument (`fn_ref`) HOF shapes.

### Closing the fn_ref gap (v3) — the targeted family moves

`combinatorial_specs()` section 19 adds **36 `fn_ref` higher-order shapes** — bodies that APPLY a
function-valued parameter, supplied in examples as an `fn_ref` to a helper built by `build_and_verify`. They
are *structurally distinct* from the eval's held-out fn_ref records (`map_with`/`apply_to`/`twice`/`compose2`/
…) so they teach the skill without leaking those answers: `apply_to_k` (`\f -> f k`), `thrice`, `compose3`,
`map_compose` (`\f g xs -> map f (map g xs)`), `filter_map_with`, `sum_with`, `reject_with`, `fold_with_k`,
and apply-f-then-builtin shapes (`map_apply_op_k`, `filter_apply_cmp_k`). Monomorphic INT typing keeps
verification robust; all verify, zero drops, default corpus byte-identical.

Same 3B, retrained on the v3 data (v2 + the 36 fn_ref shapes):

| 3B training data | write | read | total |
|---|---|---|---|
| narrow (1-12)            | 31.8% | 44.7% | 38.2% |
| + broadened (13-18)      | 41.1% | 47.5% | 44.7% |
| **+ fn_ref (19)**        | **43.0%** | **55.3%** | **48.7%** |

The targeted family moved: per-family write **`list/hof` 5% → 18%** (the rest flat, as expected — untouched).
And a spillover — **`read` +7.8 pts (47.5 → 55.3)**: the +36 fn_ref *write* examples (read training data
unchanged) exposed more higher-order surface forms, which also helped *reading* list/HOF code. Net write
+1.9, total +4.0. The fn_ref skill is now partially learned rather than flat. Still write 43% — not "speaks
Nova Lingua" — but the breadth → retrain → measure loop reliably moves the family each new coverage area
targets. Remaining frontiers: richer variant-matching, deeper recursion shapes, and a larger base / more
iters once coverage saturates.

## Notes

- **The adapter is the shippable artifact.** Unlike a closed managed fine-tune, the LoRA adapter (and the
  recipe to reproduce it) can live in the repo / commons. Next step once the signal is confirmed: pin a base
  + adapter and treat it as the reference "speaks-Nova-Lingua" checkpoint.
- **Cost: $0.** Everything here is local. The only network traffic is the one-time base-model download from
  the HF Hub. Contrast `FINETUNING.md` (OpenAI), which bills ~$10–25 but is a clean cross-check on a
  different base family.

## Addendum — GPU scale-out (2026-06-29): the residuals were a *capacity* limit

The conclusion above ("the real lever is structural coverage; a 7B base would add a few points but not
close the gap") was bounded by the small-model ceiling a CPU can train. Renting a cloud GPU (a few dollars
for the whole sweep — a full train+eval cycle drops from **~12 hr on a CPU to ~15 min**) let us run the
ladder properly, and it **revises that takeaway: model capacity is the binding lever for the hard residuals.**
The CPU path stays the $0 option; the GPU is the cost-for-speed trade when you want the bigger bases or fast iteration.

Held-out `write` (of 150, conventions-off/shots-0, 2 epochs on the broadened corpus `ftdata5`, bf16):

| base | 1.5B | 3B | 7B |
|---|---|---|---|
| **Qwen2.5-Coder** | 118 | **129 (86%)** | 127 |
| Qwen2.5 (general) | ~107 | 117 | 128 |

- **The "permanent" 1.5B residuals crack with scale.** `implies` cracks at ≥3B; `nand` (which survived every
  corpus/epoch change at 1.5B) cracks at **7B on both bases**; `concat_lists` needs a **code-pretrained** base
  (Coder gets it at 3B, the general base never does). `modulo` stays flaky (edge). So the corpus taught the
  *shapes*; capacity supplies the headroom to *apply* them.
- **Code-pretraining is a large edge at small size that converges at 7B:** Coder − general ≈ +11 (1.5B),
  +12 (3B), ~−1 (7B). Below ~7B, use Coder; at 7B the base barely matters.
- **Best config: Coder-3B** (write 129/150 = 86%, half the size of 7B). The CPU path here remains the $0
  fallback for the small end; any rented GPU box runs the same `train_lora_cpu.py` + eval (they auto-use CUDA).
- **Corpus6 update (2026-06-29, RTX 5090):** adding **family #35 (min/max bounds & clamp)** to the generator
  and retraining Coder-3B (2 seeds, 2 epochs) lifts write to **137/151 = 91%** (seed 0; seed 1 = 132) — the
  new family **closed the whole min/max/clamp write-residual cluster on both seeds** (clamp, in_range,
  max_self, min_of_list, max_list_rec, max/min_*_absorb), confirming broaden→retrain→measure at this
  capacity.
- **Corpus7 update (2026-06-29, RTX 4090) — the plateau:** **family #36 (powers & digit arithmetic via
  primitives)** robustly fixed its two targets — `square_diff` and `sum_digits` now pass **both** seeds (the
  model writes `mul a a` / div-mod recursion instead of the invented `a^2` / `show`/`digitToInt`). Best
  checkpoint write **138/151 = 91%** (seed 1; **the pinned reference**, see
  [`REFERENCE_CHECKPOINT.md`](REFERENCE_CHECKPOINT.md)). BUT seed 0 = 130, so the 2-seed mean (~134) is
  **statistically flat vs corpus6** — the corpus-breadth lever has hit diminishing returns: each family
  reliably fixes its 1–2 named tasks, but the ±10/seed noise now swamps the aggregate. (A negative result
  worth recording: fixed-base power-by-recursion was *already* covered by `rec_pow_*`, and the model still
  wrote `2 ** n` — a generalization limit, not a coverage gap, so that sub-family was dropped.)
- **Corpus7 + Coder-7B (2026-06-29, RTX 4090) — the capacity lever, confirmed.** Same `corpus7`/`ftdata7`,
  2 epochs × 2 seeds, base = Qwen2.5-Coder-7B-Instruct (7.66B params, LoRA 0.53% trainable, ~15 GB bf16,
  fits the 24 GB 4090). Result: **write 141/151 = 93.4% on BOTH seeds** (total 280/304 = 92.1% sem) — beats
  the 3B plateau (138 best / ~134 mean) and is **seed-stable** (3B swung 130↔138; 7B is 141,141). It
  **cracked `nand`** on both seeds — the residual that survived every corpus round *and* 3B — plus
  `reverse_concat`, confirming those were capacity-bound, not corpus gaps. Still failing at 7B (small, mostly
  heavily-covered noise): `nth` (still invents `error`), `member`, `reverse`, `max_list_rec`, `pow2`. **Two
  reference tiers now: Coder-3B (efficient, ~91%) and Coder-7B (best, 93.4%, stable).** Adapter
  `adapter-coder7b-c7-s1`.
- **Corpus8 re-pin (2026-07-04) — the idiom lever revises the capacity reading.** All tiers retrained
  2-seed on `corpus8` (families #37/#38, the *total idioms* for the list residuals, plus the `last`/`init`
  builtins; 157 write tasks). **Coder-7B write 149/150 = 95.5% best seed — the new best write tier**,
  beating even Coder-14B-on-corpus7's 147 ceiling at half the size; 3B's 2-seed mean rose to 90.1% with the
  seed swing halved; 14B (144/150) keeps the total/read crown (95.3–95.9% semantic) but showed write
  seed-variance 7B doesn't have. The headline revision: `foldr_with`/`member`, which corpus7 left failing
  at 7B and 14B *cracked* (hence "capacity-bound"), pass **at 7B** on corpus8 — they were **idiom-bound**;
  the bigger model had merely guessed the idiom unaided. Residual write core common to all tiers:
  `divide`/`modulo`, `pow2` (design), and `take_rec`/`drop_rec` (the list-*returning* index walks — the
  one actionable coverage gap left; #38 taught only the element-returning walk). Tiers and pins in
  [`REFERENCE_CHECKPOINT.md`](REFERENCE_CHECKPOINT.md).
