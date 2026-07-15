# Reference checkpoint — the "speaks-Nova-Lingua" model

This pins the current best fine-tuned models for *Nova Lingua*. Per the project finding that **the corpus
teaches the shapes and model capacity supplies the headroom to apply them**, the reference is a LoRA adapter
over a code-pretrained Qwen2.5-Coder base. The write/read counts below are per-pin against the eval that
round ran (the pool grows with the language — 380 tasks at c14, **416 at c21**); cross-round claims are
made on **shared tasks**, never raw totals:

| tier | base | pin | notes |
|---|---|---|---|
| **best write** | **Coder-7B** (corpus14, s1) | **176 / 185 of 189 (97.9% best, 95.5% mean)** on the 380-task eval | best ever by a wide margin; seed-1 passes **every historical residual** — its only misses are 4 long-known churn tasks. **c23-7B-s0 exactly TIES it on the 188 shared write tasks (184 = 184)** — the first post-c14 cycle to reach the pin's level — and adds the contract/fold capabilities, but a tie does not move a pin (the round-17 convention) |
| **best total / read** | **Coder-14B (corpus23, s1)** | **total semantic 400/414 = 96.6%, read 191/196 = 97.4%, write 197/206** | **re-pinned in round 23 (2026-07-15)**: beats the c21-s1 pin **+3 shared-write / +6 full-set** on the same 416-task eval, and carries family #53 (precondition trust — `divide`/`modulo` pass every c23 cycle). Hosted on Arca: `wgt_80f97246a182…` + signed eval attestation `evl_21f590f38342…`, superseding `wgt_2d1dcd3d78e6…` (resolvable, append-only); consumer `certified` → CERTIFIED |
| **efficient** | Coder-3B (corpus14, s0) | 181 / 173 of 189 (95.8% best, 93.7% mean) on the 380-task eval | best 3B ever; c23 3B seeds are −2/−6 on shared write (best post-c14 yet, up from c22's −9) — the pin stands |

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

> **The corpus14 round (2026-07-06 evening, 3B/7B 2-seed + the 14B re-baseline, RTX PRO 6000
> Blackwell) — the designed-residual list empties.** Family #45 (float report shapes — the GW5
> `to_float`/numeric-`div`/numeric-`to_string` pull) taught **all 5 new curated float rows on first
> contact at the best seed of every tier** (both 7B-s1 and 3B-s0 sweep them; the weak seeds churn
> `mean_of`/`stat_line`/`show_float` individually), and **`write/insert_sorted` — the corpus13
> residual — closed at 7B on BOTH seeds** (plus 3B-s0): it was idiom-bound, the corpus13 transformed
> walks just needed a second round of training mass, the `foldr_with` pattern repeating. Headline:
> **7B-s1 write 185/189 = 97.9%** (line-comparable old-set subset 179/183 vs the c13 pin's 173 —
> a +6 genuine gain), 3B-s0 **181/189 = 95.8%**, and the **14B re-baseline** (first retrain since
> corpus10) lands at write 181 / **total 95.8% semantic, the best total ever**, holding the read
> crown (95.5% semantic) on the current corpus. 7B-s1's only write misses: `concat`, `product`,
> `head_of`, `key_list` — all long-known per-seed churn; **for the first time no designed residual
> remains**. New read-side watch items: `read/insert_sorted` and `read/min_string` fail at every
> tier/seed (the model mis-evaluates the insert walk and the case-select on strings), and
> `read/reverse_concat` dipped at 7B/14B this round; `write/reverse_concat` churns at 3B only.

> **The corpus16 round (2026-07-08, 3B/7B 2-seed + 14B-s1, RTX PRO 6000 Blackwell) — a diagnostic
> round, NOT a re-pin: the read-side fixes work but cost write mean, so c14 stays the pin.** corpus16
> folded the round14 read-diagnosis fixes into training (insert-walk examples reordered off tail-only to
> head/middle branches, mixed-case `str_lt` comparisons in *read* position, two-list reverse+append
> shapes) plus families #46 (dispatch) / #47 (HTTP response). The eval grew 380→390 (the 10 GW3 dispatch
> tasks, 5 write + 5 read; no HTTP tasks landed in the eval). **The read watch items closed:**
> `read/insert_sorted` flipped **0/5 (every c14 run) → 4/5** (only 3b-s1 still misses) — the
> head/middle-branch read examples were the fix; `read/reverse_concat` passes **5/5**; `read/min_string`
> passes at **14B only** (still 3B/7B-resistant — the code-point-vs-alphabetical str_lt error is
> capacity-bound below 14B, unmoved by the mixed-case read rows). **But write regressed on the
> line-comparable old-380 subset:** 7B best 179/189 (mean 177) vs c14's 185/189 (mean 180.5); 3B best
> 174/189 (mean 173.5) vs c14's 181 (mean 177); 14B 179 vs 181 — a **~3–4-pt write-mean drop at every
> tier**. Part of the gap is that c14's pins caught lucky high-variance seeds (7B s1=185 vs s0=176;
> round16's seeds are *tighter*, 175–179, but don't reach the peak), but the mean is genuinely down: the
> read-focused corpus edits diluted the write signal at fixed training budget. Full-390 semantic totals:
> 14b-s1 366/390 (93.8%, best), 7b-s0 363, 7b-s1 361, 3b-s0 358, 3b-s1 354. **Decision: keep the c14
> pins; do NOT republish to Arca.** The read/insert_sorted fix is real and worth keeping — the open work
> (next round, when pulled) is to re-derive it *without* the write cost (diff corpus14→16 training rows
> for the write-perturbing change; or add epochs/steps so the read rows are absorbed without diluting
> write). New named residual: **`read/min_string` is 14B-only** (capacity-bound below 14B). Adapters +
> evals pulled to `/var/tmp/claude/round16/`.

> **Fix #1 STAGED for round17 (2026-07-08, corpus17/ftdata17) — the write-preserving read decouple.**
> Root-caused the corpus16 write regression in the code: `export_finetune` builds both the write and
> read training pairs from the same corpus rows, and `build_write_tasks` renders the *full* example
> list — so the round14 read fix, done by REORDERING each insert-walk / `_S43_IN` row's examples to put
> a branch-diverse case at `examples[-1]`, silently rewrote those rows' WRITE pairs. Fix: restore the 5
> mutated rows (`insert_sorted_int` / `insert_sorted_desc` / `insert_desc_str` / `insert_sorted_ci` and
> the `_S43_IN`-driven `before_`/`after_`/`min_vs_` rows) to their c14 examples — **proven byte-identical,
> `CHANGED(shared)=0` across all 3142 shared combinatorial rows** — and move the read fix to an explicit
> `read_example` index (sidecar in `views`, not in the hashed record; `build_read_tasks` honors it,
> default `-1`). The read pair now holds out a middle-branch insert / counterintuitive code-point compare
> (`min("Zeb","zoo")="Zeb"`, `str_lt "Zeb" "banana"=true`) with ZERO write perturbation. corpus17 = 3,409
> rows (0 gate drops), oracle 390/390, ftdata17 = 5,940 train (438 leakage-excluded). Retrain
> `/var/tmp/claude/round17_{stage,driver}.sh` (3B/7B 2-seed + 14B-s1); pod = user decision. Hypothesis:
> write recovers to c14, read/insert_sorted stays 4/5, read/min_string stays 14B-only.
>
> **RESULT (2026-07-08, RTX PRO 6000, 5 cycles) — hypothesis REFUTED on write, read fix HELD, 14B is
> the best ever.** Old-380 write: 3B 168/176 (mean 172), 7B 175/178 (mean 176.5), **14B 182** — i.e.
> round17 ≈ round16 at 3B/7B (round16 was 173.5 / 177), NOT recovered to c14 (177 / 180.5). Since
> corpus17's write pairs are provably c14-identical yet write still trained to round16 level, **the
> ~3-4pt write regression is NOT the 5-row example mutation** — it is corpus-growth dilution (the
> #46/#47/reverse families added on top of c14, at a fixed 2-epoch budget) and/or c14's pins being the
> lucky seeds (its true means were 177/180.5). The read fix held cleanly via `read_example`:
> read/reverse_concat 5/5, read/insert_sorted 3/5, read/min_string 14B-only (all comparable to round16,
> now with ZERO write-pair rewrite); write/insert_sorted even ticked to 4/5. **14B-c17-s1: write 186/194
> (95.9%), total 373/390 = 95.6% sem; old-380 total ≈ 96.0% sem, write 182** — i.e. it TIES c14's 14B
> (95.8% sem, write 181) and clearly beats round16's 14B (+5 write / +7 total). Adapters+evals in
> `/var/tmp/claude/round17/`. **Fix #1 is a keeper (carries the read fix correctly + isolated the cause by
> elimination). The indicated write lever is now TRAINING BUDGET — 3 epochs or upsample write shapes —
> not the corpus edit (round18).** All pins STAY c14 for now (14B-c17 only ties c14, so a re-pin +
> Arca republish is marginal and deferred); weights are local at `/var/tmp/claude/round17/`.
>
> **The round18 training-budget test (2026-07-09, RTX 4090, 3B/7B 2-seed, same corpus17/ftdata17,
> 3 EPOCHS) — FLAT: the third epoch does NOT recover write, and it costs the read fix. The loop is
> PARKED; all pins stay c14.** Old-380 write: 3B 174/173 (mean 173.5 vs round17's 172, c14's ~176-177),
> 7B 176/178 (mean 177 vs round17's 176.5, c14's ~179.5-180.5) — the epoch bought +0.5-1.5 write mean,
> well inside the ±10 noise floor. Full-390: 7B-s1 write 182/194 (93.8%), total 362/390 = 92.8% sem
> (round's best; still below 14B-c17's 95.6%). What the epoch *did* do: tightened seed spread (3B 8→1)
> — and **eroded the read fix: read/insert_sorted fell 3/4 → 0/4 at 3B/7B** (the single `read_example`
> held-out pair overfits away at 3 epochs; read/reverse_concat still 4/4, read/min_string still
> 14B-only). **Conclusion: the ~3-pt write-mean gap vs c14 is corpus-composition-bound at these tiers —
> neither the corpus edit (round17) nor training budget (round18) closes it, and c14's pins sit on lucky
> high seeds atop a real but small dilution cost from the #46/#47/reverse families. Write is at 93-98%
> across tiers; further crank-turning is diminishing returns. Park until a new capability pull adds
> tasks worth training on.** Adapters+evals in `/var/tmp/claude/round18/`.
>
> **The round18c measurement (2026-07-09, RTX 5090, 3B/7B 2-seed, corpus18/ftdata18, 2 epochs —
> line-comparable to round17) — family #48 is a NULL RESULT: its target shape was already solved, so
> it moved nothing, but it caused no dilution. c14 pins stay; no re-pin, no republish.** Family #48
> mass-produced the guard→`Maybe` body (`case <guard> of true => None; false => Just(<op>)`) on the
> hypothesis that its three curated instances (safe_div/safe_mod/first) under-taught it. The
> measurement **refuted that hypothesis**: those six eval tasks (read+write) pass at ALL c18 tiers/seeds
> — but they *also* passed at c14 and c17, so #48 flipped nothing. Old-380 write: 3B 175/180 (mean
> 177.5), 7B 179/177 (mean 178) — at c14 level (176 / 179.5), *above* the round17 dip (172 / 176.5), so
> the 12-spec add did **not** dilute (the round16/17 dilution concern did not reproduce for a small
> targeted family). Best full-390: 3B-s1 write 184/194 (94.8%) total 365 (93.6% sem); 7B-s0 write
> 183/194 (94.3%). Read watch (Fix #1) behaves as before: read/reverse_concat mostly passes,
> read/insert_sorted flickers (7B-s0 only), read/min_string 14B-only. **Verdict: #48 stays in the corpus
> (harmless, defensively covers a real shape at mass, and a harder future eval task in that shape would
> benefit), but it is not a re-pin — the shape had no eval-level gap to close. c14 pins stand.** The
> broader lesson reinforces round18: at 93–98% write, a new family only moves the headline if it targets
> a shape the model is actually *failing* — and the ingestion pulls, while real language capabilities,
> landed on shapes the model already writes. Adapters+evals in `/var/tmp/claude/round18c/`.
>
> **The round19 measurement (2026-07-10, RTX 4090, 3B/7B 2-seed, corpus19/ftdata19, 2 epochs — the
> GW10 `url_encode` pull's retrain, the parked loop's resume trigger) — FIRST-CONTACT CONFIRMED for
> the new capability, no re-pin: c14 pins stand and the loop goes back to PARKED.** The eval grew
> 390 → 400 (shots-0; five curated url rows). The pull's question — does family #49 supply the
> day-one mass — answers **yes**: of the ten new url tasks (write+read × 5), 7B passes 9/10 (s0) and
> 8/10 (s1), 3B 7/10 both seeds; every *query-building* shape (`query_of`/`search_url`/`param_pair`/
> `encode_all`) writes correctly at 7B, and the sole recurring miss (`write/encode_term`, the
> near-bare row) is a **near-miss, not an untaught builtin** — 7B reaches for `url_encode s` but
> decorates it (`str_concat "%" (url_encode s)`); only 3B invents a nonexistent helper. So the
> #41 failure mode (builtin never seen in training) did **not** recur — the family taught the
> operation. Stability: on the tasks shared with round18, write is 1–4 below round18's means
> (3B 176/172 vs 177/176; 7B 177/177 vs 179/181) with the usual ±10 churn and read level-to-+3 —
> i.e. the +15-spec add cost nothing real but recovered nothing either, reconfirming round18's
> conclusion that the write mean is corpus-composition-bound at 93–98%. Best runs (400-task):
> 7B-s1 write 182/199 (91.5%), total 369/400 = 92.2% sem; 7B-s0 within 1. **Verdict: family #49
> stays (it demonstrably taught the new builtin's idioms at first contact — the reason the loop
> re-armed); no adapter beats the c14 pins on shared tasks, so no re-pin, no republish. The loop
> re-parks until the next capability pull.** Adapters+evals in `/var/tmp/claude/round19/`.

> **The round20 measurement (2026-07-11 night, RTX PRO 6000 Blackwell, 3B/7B 2-seed + 14B-s1,
> corpus21/ftdata21, 2 epochs, ~1h45m ≈ $3.70 — the GW14/15 pulls' first-contact round; eval now
> 416 tasks shots-0 incl. 16 header/link tasks from families #50/#51).** First-contact was the
> **strongest ever for a pull: 7B-s0 hits 15/16 new-family tasks** (3B 10–11, 7B-s1 12, 14B 13);
> the one miss common to every tier is `write/link_target` — a hallucinated-API failure
> (`parse_link_header`, Haskell `>>=`) on the extract-between-delimiters composite, which family
> **#51 does not emit** (only the held-out curated row has it) — the next corpus touch should add
> that shape. On shared tasks: 7B-s0 is the best post-c14 7B (**+4 vs round18, +6 vs round19**)
> but −1 vs the lucky-seed c14-7B-s1 pin → 7B pin stands; 3B −7/−9 → stands; **14B-s1 beats its
> c14 pin +2 on the 378 shared tasks** (gains incl. `read/implies`, `write/checked_sub`,
> `write/sum2_spec`; losses are churn) with total semantic **396/416 = 95.2%**, read 189/197 =
> 95.9% → **the 14B best-total/read tier re-pins to c21-s1**. Recurring watch: `write/reverse`
> lost at 7B both seeds again (the c13 #44 sweep does not hold across corpora — churn, not
> regression). Adapters+evals in `/var/tmp/claude/round20/` (names `c21-*`).
>
> **The named coverage gap is closed in-corpus (2026-07-12): combinatorial family #52
> (extract-between-delimiters)** emits the split-on-opener / `tail` / null-guard /
> `str_contains`-closer / `head (str_split closer …)` composite — bracket/paren/brace pairs bare,
> the curated `<`/`>` pair only with transformed heads so `link_target`'s gold keeps
> leakage-dropping (22 training pairs carry the shape; before, zero). corpus22 = **3,225 specs, 0
> drops** (sha256 `de605b8eb32b3e5f…`), oracle 410/410 unchanged (no curated/eval change),
> ftdata22 staged at `/var/tmp/claude/ftdata22` (6,364 pairs, train 6,046/valid 318, guard
> excluded 466). **Retrain deliberately not run** (pod = user decision) — the next round measures
> whether #52 flips `write/link_target` the way #42 flipped `take_rec`/`drop_rec`.
>
> **Round 22 (2026-07-12 night, corpus22/ftdata22, RTX PRO 6000 Blackwell, 5 cycles 1h47m ≈
> $3.75): it does — `write/link_target` flips to PASS at EVERY tier and seed (5/5)**, with the
> taught #52 shape verbatim (split-on-`<` / null guard / `str_contains` / split-on-`>`, total
> through Maybe); `read/link_target` passes 4/5 (the 7B-s0 read miss is churn, not shape). **No
> pin moves.** Line-comparable old-subset write vs the c14 pins: 3B −9/−9, 7B −4 (s1)/−10 (s0),
> 14B −2 — the c14 lucky-seed gap persists (the round-20 reading holds); on the full 416-task
> eval c22-7B-s1 posts **write 194/206, total semantic 94.0% — the best post-c14 7B yet** (+2
> vs c21-7B-s0 on shared tasks), and c22-14B-s1 lands −2 vs the c21-s1 pin (which keeps the
> total/read crown at 95.2%). Verdict: family #52 did its one job; the write ceiling stays
> composition/churn-bound, consistent with the parked-loop reading. Adapters+evals in
> `/home/claude/sandbox/round22/` (names `c22-*`).

> **Round 23 (2026-07-15, corpus23/ftdata23 — families #53 precondition-trust + #54
> section-tempting-folds, from the round-22 failure mining — RTX PRO 6000 Blackwell, 7 cycles
> ≈ 3h50m ≈ $8): the cleanest corpus win the loop has produced, and the 14B re-pins.**
> `write/divide`, `write/modulo`, `write/product` pass **all seven cycles** (3B/7B 2-seed,
> 14B-s1, and both seeds of an architecturally unrelated probe base) — the
> stated-contract→bare-body idiom was structurally untaught (ftdata22 carried ZERO such
> completions; every cycle answered with a hallucinated `error` guard) and one family closed
> it at every tier. Totals (id-dedup, 414 unique): 3B 380/375 (+11/+3 vs c22), 7B 388/386
> (+12/−3; **c23-7B-s0 write 198/206 TIES the c14 pin on the 188 shared write tasks** — first
> ever — but a tie doesn't move the pin), **14B 400/414 = 96.6% — best total ever, +3
> shared-write/+6 full-set vs the c21-s1 pin → RE-PINNED** (hosted: `wgt_80f97246a182…` +
> `evl_21f590f38342…`, superseding `wgt_2d1dcd3d78e6…`; consumer `certified` → CERTIFIED).
> The same batch ran the **base-generation probe: Qwen3.5-9B** (Apache-2.0, hybrid
> Gated-DeltaNet, 2026-03) on the same corpus — write 182/187, total 372/383: **loses to
> Coder-7B by 5–16 write despite being newer and larger. Code-pretraining still beats
> generational recency for this dialect at ≤9B; the Coder pins stand until a code-pretrained
> successor ships.** Ops: the Qwen3.5 line needs `flash-linear-attention` installed or it
> trains 3.5× slower on the torch fallback (fla alone sufficed; `causal-conv1d` failed to
> build on the Blackwell image and wasn't needed); its LoRA targets must be chosen from the
> module inventory — the default Qwen2.5 list touches only 8 of its 32 attention layers.
> Durable residue unchanged in kind: `run_command` (composition depth, 0/17 across three
> rounds), `read/parse_int_maybe` (the refusal anchor transfers only to consuming position),
> `read/min_string`/`read/implies` (14B-only). Adapters+evals in
> `/home/claude/sandbox/round23/` (names `c23-*`).

Pick 7B when accuracy matters, 3B when size/latency does; 14B only when *read* accuracy is the point. The
detailed recipe below is the **3B efficient default**; the 7B differs only in `--base` (weights
`adapter-coder7b-c14-s1`, sha256 `91d8940345630806…`, seed 1; the 14B total/read-champion weights
are now `adapter-c23-14b-s1` — round 23's re-pin, **hosted on the commons** as
`wgt_80f97246a182a2c1…` (safetensors sha256 `2a8bf37c5f77ca75…`, signed eval attestation
`evl_21f590f38342c557…`, superseding the c21 record `wgt_2d1dcd3d78e6be98…`, which stays
resolvable — append-only)).

A LoRA adapter is small, but the *recipe* is what makes it a checkpoint: the run is **deterministic**
(fixed seed, greedy eval, no RNG in the data path), so this manifest reproduces the adapter bit-for-bit
on the same base + corpus. The weights themselves are gitignored (regenerable) and **hosted in the
commons** per [`spec/weights.md`](../../spec/weights.md): all three pinned tiers are published to Arca
as `wgt_` pointer records with signed eval attestations of the measured scores, blobs fetchable (and
hash-verifiable) from `https://nl.1105software.com/v0/blobs/<sha256>` —
3B `wgt_0782121ed631d02f…`, **7B `wgt_83ad513dab1e98c4…`**, 14B `wgt_95885e217035dc18…`
(records + attestations committed under [`spec/examples/`](../../spec/examples/); each c14 record
carries a `supersedes` link to its prior pin (c13's 3B/7B, c10's 14B), and the whole chain stays
resolvable — the commons is append-only, so a re-pin is a new record, not an overwrite).

## The pin

| | |
|---|---|
| **Base model** | `Qwen/Qwen2.5-Coder-3B-Instruct` (Apache-2.0) |
| **Method** | LoRA, r=16, α=32, dropout=0.05, targets = all attn+MLP proj |
| **Training** | 2 epochs, **seed 0**, bf16, `--max-seq-len 512`, lr 2e-4, grad-accum 8 (RTX PRO 6000 Blackwell) |
| **Trainer** | [`train_lora_cpu.py`](train_lora_cpu.py) (auto-uses CUDA when present) |
| **Corpus** | `corpus14.jsonl` — 3,375 examples / 3,139 combinatorial specs, **45 template families** (incl. #42 list-returning index walks, #43 sort/case, #44 bare-reverse reinforcement, #45 float report shapes) (`gen_corpus.py --combinatorial`) · sha256 `dc893c505caa0c61…` |
| **Train split** | `ftdata_c14/` — 5,885 train / 309 valid, **conventions-off, curated eval held out** (`export_finetune.py --holdout-corpus`; 428 eval-matched tasks excluded) · `train.jsonl` sha256 `30cc0d35fe30bb71…` |
| **Grading** | [`eval_harness.py`](eval_harness.py) `--conventions off --shots 0`, curated set held out of training |
| **Adapter weights** | `adapter-coder3b-c14-s0` (regenerable; gitignored). Local copy: `/var/tmp/claude/round14/adapter-coder3b-c14-s0/adapter_model.safetensors`, **sha256 `a9bba22e6334b6f3388c38fa018f1508e4e3f576924da80c1f6e7ac131e934a9`** (LoRA r16/α32/dropout0.05, targets = all attn+MLP proj — matches this pin) |

## Measured result (held out, conventions-off, shots-0)

From `coder3b-c14-s0_eval.jsonl` (the 2026-07-06 corpus14 GPU run, **seed 0** — the best 3B checkpoint,
on the 380-task eval that includes the expressiveness-phase string/map/JSON tasks, the sort/case rows,
and the GW5 float rows):

| kind | surface-exact | semantic | n |
|---|---|---|---|
| **write** | **181 / 189 (95.8%)** | 181 / 189 | 189 |
| read | 161 / 179 (89.9%) | 161 / 179 (89.9%) | 179 |
| assemble | 12 / 12 (100%) | 12 / 12 (100%) | 12 |
| **total** | **354 / 380 (93.2%)** | 354 / 380 (93.2%) | 380 |

Seed 1 of the same run scored write 173/189 (the 2-seed mean is **93.7%** — the best 2-seed 3B mean yet).
The 7B tier (same recipe, `--base Qwen/Qwen2.5-Coder-7B-Instruct`, weights `adapter-coder7b-c14-s1`) is
**185/176 of 189 (97.9% best)** — the number to quote for the project's best write. base `write` is 0% —
the adapter is the whole signal.

## Reproduce / regenerate the weights

This box has no GPU, so a 3B base is a GPU step. The portable recipe:

```bash
# 1. build the leakage-guarded SFT split (local, $0)  [corpus14 = --combinatorial regen of gen_corpus.py]
python3 tooling/eval/export_finetune.py --corpus corpus14.jsonl --conventions off --shots 0 \
    --holdout-corpus tooling/corpus/corpus.jsonl --mlx-data ftdata14

# 2. train (the SAME script runs on CPU or GPU; on GPU it auto-selects CUDA+bf16)
python3 tooling/eval/train_lora_cpu.py --train ftdata14/train.jsonl \
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
m = HFModel("Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/round14/adapter-coder3b-c14-s0")  # base::adapter
task = SimpleNamespace(system="You write Nova Lingua function records.",
                       user="Write a function record for: double a natural number.")
print(m.answer(task))
```

Or drive it straight through the harness (loads the adapter, prompts, and grades every answer with
`nl-validator` — the trustworthy way to *use it and see it's right* at once):

```bash
NL_HF_DTYPE=bfloat16 python3 tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/var/tmp/claude/round14/adapter-coder3b-c14-s0 \
    --conventions off --shots 0            # add --tasks write --limit N for a quick subset
```

The `hf:<base>::<adapter>` spec routes to `HFModel`; `mlx:<base>::<adapter>` routes to Apple MLX
(`MLXModel`). Base model and adapter both download/load from the HF cache or a local path. **base `write`
is 0%** — the adapter is the entire signal, so it must be present.

## Verify the pinned number (GPU)

The `write 181/189` figure was measured once on the training pod (now terminated). The local weights
above reproduce it, but a full 380-task CPU eval of a 3B model is multi-hour — run the *verification* on a
rented GPU instead (see the local-only `RUNPOD.md`), where a full held-out eval of the existing adapter is
~5 min. On a fresh pod (base cache warmed, repo + `adapter-coder3b-c14-s0` + `ftdata14` uploaded to `/root`;
NB the repo clone needs the corpus14 `tooling/corpus/corpus.jsonl` — the eval pool):

```bash
# re-evaluate the EXISTING pinned adapter (no retrain) against the held-out curated set
NL_HF_DTYPE=bfloat16 python -u repo/tooling/eval/eval_harness.py \
    --model hf:Qwen/Qwen2.5-Coder-3B-Instruct::/root/adapter-coder3b-c14-s0 \
    --conventions off --shots 0 --out /root/verify_c14_s0_eval.jsonl
```

Expect `write` ≈ 181/189 (seed-0 pin). A full retrain-then-eval (confirms the *recipe*, not just the
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

**The residual write core after corpus14 is EMPTY** — a first. `write/insert_sorted` (corpus13's
named residual) closed at 7B on both seeds plus 3B-s0, confirming the `foldr_with` pattern once
more: an idiom the corpus already carries closes on the *next* round's training mass. On the best
seed (7B-s1) every historically-named residual passes — `modulo`, `pow2`, `is_int_string`,
`insert_sorted`, the full reverse and GW5 float families — and its only 4 misses (`concat`,
`product`, `head_of`, `key_list`) are long-known per-seed churn. What's left is churn management
and the **read side**: `read/insert_sorted` and `read/min_string` fail at every tier/seed (the
model mis-executes the insert walk / string case-select when *reading*), and `read/reverse_concat`
dipped at 7B/14B this round. `implies`, `concat_lists`, `nand` (older residuals) stay solved.

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
> `to_float`/`show_float`/`half_of`/`mean_of`/`stat_line`; the corpus14 round measured the tiers
> on that set the same evening (the current tier table; old-set-comparable subset figures: 7B
> c14-s1 holds 179/183 vs c13's 173, 3B c14-s0 175/183 vs 171 — both genuine gains on the shared
> pool). The 14B tier is now line-comparable too (same corpus, same eval set as the others, for
> the first time since corpus10).
