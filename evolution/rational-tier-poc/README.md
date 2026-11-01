# An exact rational tier over the integer tier's certified `gcd`: cross-module composition by content address, and what the contracts let the prover conclude

- **Status:** published (2026-10-30). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-30, last updated 2026-10-31 (round 7 — uniqueness of lowest terms PROVED by the staged rung)
- **Scope:** `tooling/validator` (the prover's contract composition, the divisibility rung, the
  generative checker), `evolution/integer-tier-poc` (its `gcd` record, applied by address),
  `spec/evaluation.md`.
- **Provenance:** rounds 1–5 on upstream commit `e16a8f0` (nl-validator 0.1.5 in-tree; the `v0.1.5`
  tag sits two fixes earlier, on `fe7e16b`); round 6 on `e7c307d` (nl-validator 0.1.7 in-tree, after
  the measured-descent rung — no validator change for this round, the records changed); round 7
  on `d566569` (nl-validator 0.1.7 in-tree after the witness rung and the staged rung `9be3a64`,
  the depth-0 restriction `4590d8d` and the tuple-sorted derived cache `d566569`); the integer
  tier's `gcd` at `fn_7875d6be2650b6699…` for rounds 1–6 (its eleventh-follow-up record, with the
  greatest-divisor law) and, from round 7, the integer tier's round-16 `gcd` at
  `fn_29c0f131454ed433b…` (six laws, among them Euclid's step by unfolding) with its `ext_gcd` at
  `fn_254d8d14adb94bc38…` (the Bézout post, the link post `fst(result) = gcd(a, b)`, and Euclid's
  lemma with the quotient named); z3 4.13.3 (Debian/Ubuntu package);
  python 3.14 for the builder, whose reference values are Python's exact `fractions.Fraction`; one
  laptop (Ubuntu 26.04).
- **Resolution:** —

## Summary

Fourteen pure records — `rat_normalize` (sign to the numerator, both components divided by the
integer tier's `gcd`), negation, reciprocal, add/sub/mul, equality and order by
cross-multiplication, floor, and five checker records for the algebra — were authored in surface
syntax over `(int, int)` pairs and put through the gauntlet, seven times, with the validator
changing between rounds. **Round 7: 14/14 run and certify; every declared contract is SOUND (12
posts), among them the module's headline fact as a post on `rat_normalize` — the result is in
lowest terms, `gcd(|p|, q) = 1`, stated by applying `gcd` by content address inside the predicate
and proved through `gcd`'s greatest-divisor law from the other module — and the value-preserving
identities `fst(result)·(b·d) = (a·d + c·b)·snd(result)` on the arithmetic records, each derived
from `rat_normalize`'s own proved law at the body's application. Cross-module composition of
contracts, the thing this module exists to test, works, and it now carries a contract that itself
applies a callee.** Laws: **21 PROVED / 1 UNKNOWN / 0 UNSUPPORTED** of 22 (round 6: 19 / 2 / 0 of
21; round 5: 18 / 2 / 0 of 20; round 2: 14 / 6 / 0); all 22 held generatively on the bounded
domain with nothing skipped, and all 22 held under the wide sampler, where one law reports six of
200 cases skipped at the evaluator's 128-bit limit. **Round 7 (2026-10-31) proved uniqueness of
lowest terms in the tool**: the checker law `equality_is_normal_form_equality` — equal as values
⇔ equal normal forms — is PROVED by the prover's new STAGED rung, exactly the shape round 6's
hand-measurement said it needed: 100 conclusions drawn by refuting the other disjuncts of
contract-law instances one at a time over the two branches of the negated goal, then a final query
over uninterpreted `div`/`mod` with the witness terms abstracted. The contract-law instances come
from the integer tier's `ext_gcd` — applied by nothing in this module, reached as a WITNESS callee
because its post names `gcd` by address — and from `rat_normalize`'s own laws `cross` (round 6)
and the new `cross_abs` (the same in absolute values, the form Euclid's lemma's premise reads).
The one law still UNKNOWN is additive associativity, which composes three normalizations; its
missing piece is now uniqueness *as a law on `rat_normalize`* that callers inherit ("Still open").

Getting from round 2 to round 5 cost **one prover rung and three validator defects**, all
in-tree. The rung (dffa6e2, ed76c0a): the divisibility rung reaches a *callee's* laws, with
pairwise **products** of the goal's gcd terms as free-variable witnesses and a scaling lemma `e | x
⇒ e·g | g·x` — exactly the `e·g` the round-2 hand proof said was missing. The defects, each found
by the next round because a post that *applies a callee by address* was new ground: a callee's
contract instance at a pattern-bound argument was stated outside the pattern's scope (`unknown
constant p`, 8785d85); a caller's preamble emitted `rat_normalize`'s axioms before `gcd`'s
declaration (`unknown constant callee_7875…`, cf72df4); and a caller's contract set came from its
*body's* callees only, so `rat_inv`'s law read UNSUPPORTED ("`gcd`'s certified contract is not
available here") and the arithmetic records' value posts fell to UNVERIFIABLE until the set closed
over the callees a contract applies (e16a8f0). One cost stands as measured: the complexity fold is
UNVERIFIABLE for every record that passes a product to a callee (7 of 14).

## Provenance of the inputs

| input | pinned at |
|---|---|
| `nl-validator` | rounds 1–5: 0.1.5 in-tree, commit `e16a8f0`; round 6: `e7c307d`; round 7: `d566569` (0.1.7 in-tree + the witness and staged rungs); the laws proved with z3 4.13.3 |
| the integer tier's `gcd` | rounds 1–6: `fn_7875d6be2650b6699…`; round 7: the round-16 `gcd` `fn_29c0f131454ed433b…` — copied record and body into `records/` so `--records` resolves the cross-module `fn_ref`; the address is the same, so publishing it again is idempotent |
| the integer tier's `ext_gcd` | round 7: `fn_254d8d14adb94bc38…`, copied likewise — no record here applies it; it is in the view as a WITNESS (its post `fst(result) = gcd(a, b)` names `gcd` by address, so the prover instantiates its Bézout post and Euclid's lemma at this module's `gcd` applications) |
| reference values | `fractions.Fraction` (CPython 3.14): every worked example is recomputable |
| the builder | `build_records.py` here — re-mints deterministically; rerun after any validator change |

## What was measured

Builder round 7 (`records/REPORT.{json,md}`; the log in the author's sandbox), 26.8 min, z3 4.13.3
(round 6 took 98.3 min, round 5 55.7 min, round 2 30.6 min; each round's additions — the
lowest-terms post and the rung's 30 s budget, then the `cross` law's self-contract derivation and
the two checkers' longer ladders over a richer contract set — are where the time went; round 7 is
faster than round 6 despite two more rungs because the derived-contract cache now keeps the
tuple-sorted contracts, which every process of a round had been re-deriving):

| record | signature | contracts | laws (prove) | generative |
|---|---|---|---|---|
| `rat_normalize` | `int int → (int, int)`, pre `d ≠ 0` | `snd ≥ 1`; `n = 0 ⇒ result = (0, 1)`; **`gcd(|fst|, snd) = 1`** — all SOUND | `value_preserved` **PROVED**; `idempotent` **PROVED** [Euclid] (round 2: UNKNOWN); `cross` **PROVED** from self's certified contract (round 6: `n·e = m·d ⇒ p·s = r·q`, the first half of uniqueness; its static example check is UNVERIFIABLE — a two-application law over four variables has no worked-example pair to read it off); `cross_abs` **PROVED** likewise (round 7: `|p|·s = |r|·q`, the form Euclid's lemma's premise `(|p|·s) mod q = 0` reads, since `gcd` takes nats) | EXHAUSTIVE 81 / 81; HELD 400 / 400 |
| `rat_coprime_holds` | checker | — | `lowest_terms` **PROVED** (round 1: UNSUPPORTED; round 2: UNKNOWN) | EXHAUSTIVE 81 |
| `rat_neg` | | `snd = d` SOUND | involution ×2 **PROVED** | EXHAUSTIVE 81 ×2 |
| `rat_inv` | pre `n ≠ 0` | `fst·n = d·snd` SOUND (round 4: UNVERIFIABLE) | `denominator_positive` **PROVED** (round 4: UNSUPPORTED) | EXHAUSTIVE 81 |
| `rat_add` | 4 ints, pre `b, d ≥ 1` | `snd ≥ 1`, `fst·(b·d) = (a·d + c·b)·snd` both SOUND | `commutative` **PROVED** | HELD 400 |
| `rat_sub` | | both SOUND | `self_difference_is_zero` **PROVED** | EXHAUSTIVE 81 |
| `rat_mul` | | both SOUND | `commutative`, `zero_annihilates` **PROVED** | HELD 400, EXHAUSTIVE 729 |
| `rat_eq` | 4 ints → bool | `result = (a·d = c·b)` SOUND (round 6) | `reflexive`, `symmetric` **PROVED** | EXHAUSTIVE 81, HELD 400 |
| `rat_lt` | pre `b, d ≥ 1` | — | `irreflexive`, `asymmetric` **PROVED** | EXHAUSTIVE 81, HELD 400 |
| `rat_floor` | pre `d ≥ 1` | — | `lower_bound`, `upper_bound` **PROVED** | EXHAUSTIVE 81 ×2 |
| `rat_add_zero_holds`, `rat_mul_one_holds` | checkers | — | `add_zero`, `mul_one` **PROVED** by unfolding the callees' definitions (round 2: UNKNOWN) | EXHAUSTIVE 81, 81 |
| `rat_eq_normalize_holds` | checker | — | `equality_is_normal_form_equality` **PROVED** [STAGED: 100 conclusions over 2 branches of the negated goal; the finals over uninterpreted `div`/`mod`] (rounds 2–6: UNKNOWN — uniqueness of lowest terms, Euclid's lemma) | HELD 400 |
| `rat_add_assoc_holds` | checker | — | `associative` UNKNOWN — three normalizations composed; needs uniqueness as a law callers inherit (below) | HELD 400 |

Totals: 14/14 run PASS, 14/14 certified, termination SOUND 14/14, complexity SOUND 7 /
UNVERIFIABLE 7 (every record passing a product to a callee: add/sub/mul and the checkers over
them, plus `rat_coprime_holds`), contracts 12 SOUND / 0, laws 21 PROVED / 1 UNKNOWN / 0
UNSUPPORTED, 22/22 held (bounded; no skipped cases anywhere — every law is guarded, `d == 0 ||`,
`b < 1 ||`, and the guards decide) and 22/22 held (`--wide`, 200 cases sampled across magnitudes;
`associative` reports 194 held and 6 skipped — three-fraction sums at numerators near 10¹⁷
overflow the evaluator's 128-bit integers, and the skipped-case report says so rather than
counting them as held).

**The rounds, as a record of the gauntlet.** Round 1: `lowest_terms` UNSUPPORTED (the scoping
defect). Round 2 (published first): 14 / 6 / 0, the hand proof of lowest terms recorded below.
Round 3: the lowest-terms post authored; every caller of `rat_normalize` hit the declaration-order
defect. Round 4: `rat_normalize` post SOUND, `lowest_terms` and `idempotent` PROVED; `rat_inv`
UNSUPPORTED and the value posts UNVERIFIABLE (the contract-set defect). Round 5: 18 / 2 / 0 of 20.
Round 6 (98.3 min, log `rational-build-round6.log`, nl-validator at `e7c307d`, no validator
change): the `cross` law PROVED from `rat_normalize`'s own certified contract (`value_preserved`
at both applications, the cancellation by the solver), `rat_eq`'s decision post SOUND — the table
above; the two checkers stay UNKNOWN, now for the measured reason recorded under "What is argued".
Round 7 (26.8 min, log `rational-build-round7.log`, nl-validator at `d566569` — the witness rung,
the staged rung, the retry restricted to the record under proof, the derived cache keeping
tuple-sorted contracts; the integer tier's round-16 `gcd` and `ext_gcd` copied in): the
`cross_abs` law authored on `rat_normalize` and PROVED from self's contract, and
`equality_is_normal_form_equality` **PROVED** by the staged rung — uniqueness of lowest terms, in
the tool. The round's first two starts were killed (every contract derivation ran the new retry,
then every process re-derived the tuple-sorted contracts) and its third froze the author's laptop
hard — the run that finished ran inside a memory-capped cgroup (9 GiB, swap off) and peaked at
817 MB; the freeze did not reproduce and its cause is unconfirmed (the user's kernel log was not
readable from the build account).

**The lowest-terms proof, by hand and now by the prover.** Let `g = gcd(|n|, |d|)`, `p = n div g`,
`q = d div g`, `e = gcd(|p|, q)`. Then `e | p` and `e | q`, so `e·g | n` and `e·g | d` (scaling),
so `e·g | g` by the greatest-divisor law at `d = e·g`, so `e·g ≤ g`, so `e = 1`. The divisibility
rung instantiates a law's free variable over the goal's gcd terms and, now, their pairwise
products; `e·g` is one, and z3 closes the query in about 3 s (10 s under z3 4.16) with the
transitivity, antisymmetry, scaling and bound lemmas, each discharged by the solver first.
Measured before building: the product witness and the scaling lemma are load-bearing; the divisor
bound, `abs` commuting and the Euclidean identities z3 derives itself.

**A correction to round 2's analysis.** Round 2 filed `add_zero` and `mul_one` among the laws
waiting on uniqueness. They are not: with `0 = (0, 1)` and `1 = (1, 1)`, `x + 0` unfolds to
`normalize(a·1 + 0·b, b·1)`, which is `normalize(a, b)` by arithmetic alone, and the prover's
unfolding rung now says so. They read UNKNOWN in round 2 and PROVED in round 5 on the same
records; which of the intervening validator changes flipped them was not isolated.

## What is argued

- **Contracts compose across modules exactly as within one — including a contract that applies a
  callee.** `gcd` was never re-authored here; its record and body were copied under their own
  address, and every fact the rational records needed from it arrived through the linked view's
  re-derivation (and the derived-contract cache). The lowest-terms post goes one step further: a
  predicate on one module's record applies another module's function by address, and the prover
  must then carry *that* callee's contract into every record that applies the first. Three places
  in the validator had assumed contracts were callee-free; each is now a test.
- **A contract pins values, not functions — and the value posts are the right contract.** The two
  checkers UNKNOWN through round 6 compose records whose *values* are certified equal; two pairs
  with the same value are equal only if both are in lowest terms (a contract since round 4) and
  lowest terms are unique (round 7). That was the honest shape of the gap: not "the prover is weak
  on composition" but "one number-theoretic fact is missing, and two laws wait on it".
- **The next rung is Euclid's lemma, and the integer tier already holds its key.** Uniqueness: if
  `p/q = r/s` with both in lowest terms and `q, s ≥ 1`, then `p·s = r·q`, so `q | p·s`, so `q | s`
  by Euclid's lemma (`gcd(p, q) = 1 ∧ q | p·s ⇒ q | s`); symmetrically `s | q`; so `q = s` and
  `p = r`. Euclid's lemma itself is a linear step from Bézout: `u·p + v·q = 1` gives `s = u·(p·s)
  + v·q·s = q·(u·r + v·s)`. The integer tier's `ext_gcd` carries the Bézout identity as a SOUND
  post, so the rung is an instance of an existing cross-module contract at the goal's terms, not a
  new lemma — the same pattern as the product witness. Measured need: two laws.
- **Uniqueness is a STAGED proof, measured (round 6, 2026-10-31).** Before building the Bézout
  rung, the whole chain was hand-measured in z3 (QF_NIA, `~/sandbox/uniq_*.smt2`; z3 4.13.3).
  Each step closes alone: cancellation `p·b = a·q ∧ r·d = c·s ∧ a·d = c·b ⇒ p·s = r·q` (0.13 s);
  Euclid's lemma with its quotient NAMED, `s = q·(±u·r + v·s)` from `|p|·u + q·v = 1` and
  `p·s = r·q` (0.06 s); antisymmetry over products `s = q·A ∧ q = s·B ∧ q, s ≥ 1 ⇒ q = s` (0.01 s);
  the final cancellation (0.01 s); the premise `(|p|·s) mod q = 0` from `p·s = r·q` (0.39 s); and
  the final query with the Euclid-lemma instances' CONCLUSIONS as bare facts over opaque witnesses
  (0.02 s). What does NOT close, at 60 s: the one-shot query (everything together); the same with
  the witnesses named; the law instances in `mod`/`div` form beside the Bézout facts; the same
  without Bézout; the final query with the `div` terms live instead of abstracted; and the
  pure-`mod` form of Euclid's lemma (`gcd(a, b) = 1 ∧ (a·s) mod b = 0 ⇒ s mod b = 0`) from the
  Bézout identity alone — the solver does not find the quotient, which is exactly what the
  Euclidean rung supplies elsewhere. The one shape that closes with a certificate's facts
  present is the pure-`mod` instances WITHOUT the noise (13.7 s); with the cancellation premises,
  the Bézout and link facts beside them — a real certificate — it is unknown again. So the rung
  this needs is not one more instance but a STAGED query: each law instance's premise discharged
  in its own query under the hypotheses, the conclusions then stated as facts with the witness
  terms (`div`, the Bézout products) abstracted — the discipline of the stepwise Euclidean checker,
  generalized from its own instances to contract-law instances with hypothesis-dependent
  premises. Two more pieces come with it: the law must be reachable from a record that applies
  nothing of `ext_gcd` (a witness-callee lookup — a view record whose post names `gcd` by address,
  instantiated at `gcd`'s applications), and `ext_gcd` needs the link post `fst(result) = gcd(a, b)`
  (UNVERIFIABLE today by numeric induction: the step needs `gcd(a, b) = gcd(b, a mod b)`, which is
  `gcd`'s own definition and no law states — a one-line unfolding law on `gcd`, whose re-minting
  cascades to every record that applies it). Round 6 took what the measurements already support:
  the first half of uniqueness as a certified law.
- **Uniqueness PROVED in the tool, by the rung the measurement described (round 7, 2026-10-31).**
  The validator gained all three pieces (`9be3a64`, `4590d8d`, `d566569`): the WITNESS rung — a
  view record whose post says `fst(result) = G(params)` is applied at `G`'s applications, so
  `ext_gcd`, applied by nothing here, brings its Bézout post and its Euclid's-lemma law to every
  `gcd` term of the goal (the integer tier's round 16 gave `ext_gcd` the link post, SOUND, via
  `gcd`'s unfolding law `euclid_step`); the STAGED rung — the negated goal split into branches,
  each contract-law instance's conditional conclusion established by refuting its other disjuncts
  one at a time under tiers of facts, the conclusions re-stated as facts with the `div` and Bézout
  witness terms abstracted, every query grounded, the finals grown from the most recent
  conclusions; and the derivation's self-contract phase, so a law the CLI proved on
  `rat_normalize` (`cross`, `cross_abs`) is in every caller's contract. On this module's real
  records the checker's law closes with 100 conclusions over 2 branches. Two costs were measured
  on the way and fixed before the round finished: the retry ran inside every contract derivation
  (minutes per UNKNOWN law per process — now depth 0 only, and a law only staging proves is not a
  contract law for callers), and the tuple-sorted contracts were never cached (727 s cold / 0.1 s
  warm on `rat_inv` once they were).
- **Associativity is the same fact one level up.** `(x + y) + z = x + (y + z)` compares two
  normalizations of values the value posts certify equal; the checker record applies
  `rat_normalize` through `rat_add`, so the staged rung's witness instances sit two applications
  away and the query stays UNKNOWN. What callers lack is uniqueness *as a law on `rat_normalize`*
  (`n·e = m·d ⇒ self(n, d) = self(m, e)`), which the self-contract phase would then hand to
  `rat_add` and on to the checker — the next rung, recorded, not built.
- **Complexity through products is a fold limit, not a defect.** The class fold reads a callee's
  cost only for size-bounded arguments; `normalize(a·d + c·b, b·d)` is `O(log(a·d + c·b))`, which
  is `O(log n)` in the inputs' magnitude but not in the fold's vocabulary. Stated, not changed.
- **The gauntlet is the point.** Five rounds of one module found three defects in a prover with
  640-odd tests, each in a place no unit test had reached because no record had yet applied a
  callee inside a contract; rounds 6 and 7 forced two prover rungs that the hand-measurement had
  specified before they were built. The module's records changed only to add laws after round 3;
  the validator did the moving.

## What worked well

- The integer tier's work carried over whole: `gcd`'s posts and the greatest-divisor law were
  simply there, by address, and the product witness built for the integer tier's `gcd`
  associativity proved the rational tier's lowest terms a few hours later.
- Hand-measuring each rung with z3 before building it (the four lemma variants, which are
  load-bearing) kept the rung small: products of the goal's terms, bounded, and one scaling lemma.
- The solver's own error text pointed at each defect (`unknown constant p`, `unknown constant
  callee_7875…`, "its certified contract is not available here"); each fix came with a unit test
  reproducing the shape small, and the derived-contract cache kept the later rounds cheaper.
- The skipped-case report made both samplers honest: zero skips on the bounded domain, six named
  overflow skips under `--wide`, with the offending inputs printed.

## Still open

- **Additive associativity** (`associative`, the last UNKNOWN): author uniqueness as a law on
  `rat_normalize` itself (`n·e = m·d ⇒ self(n, d) = self(m, e)` — the staged rung proves it there
  as it did on the checker) so the self-contract phase carries it into `rat_add`'s contract and
  the checker's query becomes two value-post instances plus that law. Not built; the checker's
  own law stays as the external re-check.
- **Uniqueness of lowest terms** — CLOSED in round 7 (`equality_is_normal_form_equality` PROVED
  by the staged rung over `ext_gcd`'s witness instances). Kept here as the record of what it took:
  the staged query, the witness-callee lookup, and `ext_gcd`'s link post via `gcd`'s unfolding law.
- **Complexity through product arguments**: seven UNVERIFIABLE folds; a magnitude-based size
  measure for `log` classes would decide them. Driver-gated.
- Which validator change flipped `add_zero`/`mul_one` from UNKNOWN to PROVED was not isolated.
- A Gaussian-integer tier over these pairs was the natural next module and now exists
  (`evolution/gaussian-tier-poc`, 2026-10-30); a Gaussian-rational tier over both remains open.

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/rational-tier-poc/build_records.py            # mint + gauntlet (needs z3; ~27 min, ≤1 GB)
python3 tooling/commons-node/publish_records.py evolution/rational-tier-poc/records --sign <seed>
```

No credential or network is needed for the builder; the publish step needs a node.
