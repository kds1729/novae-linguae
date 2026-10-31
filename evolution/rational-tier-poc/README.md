# An exact rational tier over the integer tier's certified `gcd`: cross-module composition by content address, and what the contracts let the prover conclude

- **Status:** published (2026-10-30). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-30, last updated 2026-10-31 (round 6 — the cross law; uniqueness measured)
- **Scope:** `tooling/validator` (the prover's contract composition, the divisibility rung, the
  generative checker), `evolution/integer-tier-poc` (its `gcd` record, applied by address),
  `spec/evaluation.md`.
- **Provenance:** rounds 1–5 on upstream commit `e16a8f0` (nl-validator 0.1.5 in-tree; the `v0.1.5`
  tag sits two fixes earlier, on `fe7e16b`); round 6 on `e7c307d` (nl-validator 0.1.7 in-tree, after
  the measured-descent rung — no validator change for this round, the records changed); the
  integer tier's `gcd` at `fn_7875d6be2650b6699…` (its
  eleventh-follow-up record, with the greatest-divisor law); z3 4.13.3 (Debian/Ubuntu package);
  python 3.14 for the builder, whose reference values are Python's exact `fractions.Fraction`; one
  laptop (Ubuntu 26.04).
- **Resolution:** —

## Summary

Fourteen pure records — `rat_normalize` (sign to the numerator, both components divided by the
integer tier's `gcd`), negation, reciprocal, add/sub/mul, equality and order by
cross-multiplication, floor, and five checker records for the algebra — were authored in surface
syntax over `(int, int)` pairs and put through the gauntlet, six times, with the validator
changing between rounds. **Round 6: 14/14 run and certify; every declared contract is SOUND (12
posts), among them the module's headline fact as a post on `rat_normalize` — the result is in
lowest terms, `gcd(|p|, q) = 1`, stated by applying `gcd` by content address inside the predicate
and proved through `gcd`'s greatest-divisor law from the other module — and the value-preserving
identities `fst(result)·(b·d) = (a·d + c·b)·snd(result)` on the arithmetic records, each derived
from `rat_normalize`'s own proved law at the body's application. Cross-module composition of
contracts, the thing this module exists to test, works, and it now carries a contract that itself
applies a callee.** Laws: **19 PROVED / 2 UNKNOWN / 0 UNSUPPORTED** of 21 (round 5: 18 / 2 / 0 of
20; round 2: 14 / 6 / 0); all 21 held generatively on the bounded domain with nothing skipped, and
all 21 held under the wide sampler, where one law reports six of 200 cases skipped at the
evaluator's 128-bit limit. The two UNKNOWNs are one fact: **uniqueness of lowest terms** (Euclid's
lemma), which is what "equal values have equal normal forms" and `(x + y) + z = x + (y + z)` reduce
to once the value posts are in place. Round 6 (2026-10-31) proved its first half as the law
`cross` on `rat_normalize` — `n·e = m·d ⇒ p·s = r·q` for the two normal forms, from the record's
own certified contract — gave `rat_eq` its decision as a post, and hand-measured the second half
(Euclid's lemma) step by step: each step closes in under a second alone and no one-shot query
closes at all, which fixes the shape of the rung still missing (a staged query; "What is argued").

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
| `nl-validator` | 0.1.5 in-tree, commit `e16a8f0`; the laws proved with z3 4.13.3 |
| the integer tier's `gcd` | `fn_7875d6be2650b6699…` — copied record and body into `records/` so `--records` resolves the cross-module `fn_ref`; the address is the same, so publishing it again is idempotent |
| reference values | `fractions.Fraction` (CPython 3.14): every worked example is recomputable |
| the builder | `build_records.py` here — re-mints deterministically; rerun after any validator change |

## What was measured

Builder round 6 (`records/REPORT.{json,md}`; the log in the author's sandbox), 98.3 min, z3 4.13.3
(round 5 took 55.7 min, round 2 30.6 min; each round's additions — the lowest-terms post and the
rung's 30 s budget, then the `cross` law's self-contract derivation and the two checkers' longer
ladders over a richer contract set — are where the time goes):

| record | signature | contracts | laws (prove) | generative |
|---|---|---|---|---|
| `rat_normalize` | `int int → (int, int)`, pre `d ≠ 0` | `snd ≥ 1`; `n = 0 ⇒ result = (0, 1)`; **`gcd(|fst|, snd) = 1`** — all SOUND | `value_preserved` **PROVED**; `idempotent` **PROVED** [Euclid] (round 2: UNKNOWN); `cross` **PROVED** from self's certified contract (round 6: `n·e = m·d ⇒ p·s = r·q`, the first half of uniqueness; its static example check is UNVERIFIABLE — a two-application law over four variables has no worked-example pair to read it off) | EXHAUSTIVE 81 / 81; HELD 400 |
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
| `rat_eq_normalize_holds`, `rat_add_assoc_holds` | checkers | — | UNKNOWN ×2 — uniqueness of lowest terms (round 6: the first half is now `cross`; the rest is Euclid's lemma, measured to need a staged query — below) | HELD 400, 400 |

Totals: 14/14 run PASS, 14/14 certified, termination SOUND 14/14, complexity SOUND 7 /
UNVERIFIABLE 7 (every record passing a product to a callee: add/sub/mul and the checkers over
them, plus `rat_coprime_holds`), contracts 12 SOUND / 0, laws 19 PROVED / 2 UNKNOWN / 0
UNSUPPORTED, 21/21 held (bounded; no skipped cases anywhere — every law is guarded, `d == 0 ||`,
`b < 1 ||`, and the guards decide) and 21/21 held (`--wide`, 200 cases sampled across magnitudes;
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
  checkers still UNKNOWN compose records whose *values* are certified equal; two pairs with the
  same value are equal only if both are in lowest terms (now a contract) and lowest terms are
  unique (not yet). That is the honest shape of the gap: not "the prover is weak on composition"
  but "one number-theoretic fact is missing, and two laws wait on it".
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
- **Complexity through products is a fold limit, not a defect.** The class fold reads a callee's
  cost only for size-bounded arguments; `normalize(a·d + c·b, b·d)` is `O(log(a·d + c·b))`, which
  is `O(log n)` in the inputs' magnitude but not in the fold's vocabulary. Stated, not changed.
- **The gauntlet is the point.** Five rounds of one module found three defects in a prover with
  640-odd tests, each in a place no unit test had reached because no record had yet applied a
  callee inside a contract. The module's records never changed after round 3; the validator did.

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

- **Uniqueness of lowest terms** (`equality_is_normal_form_equality`, `associative`): the first
  half is now the certified law `cross` (round 6); the second half — `p·s = r·q` with both pairs in
  lowest terms ⇒ `p = r ∧ q = s` — is Euclid's lemma, and the measurements above fix the shape of
  the rung it needs: a STAGED query over contract-law instances (premises discharged apart,
  witnesses abstracted), a witness-callee lookup bringing `ext_gcd` in at `gcd`'s applications,
  and the link post `fst(ext_gcd(a, b)) = gcd(a, b)` (which needs `gcd`'s unfolding law
  `gcd(a, b) = gcd(b, a mod b)`). Measured need: two laws; every step of the hand proof closes in
  under a second in isolation, none of the one-shot forms closes in 60 s.
- **Complexity through product arguments**: seven UNVERIFIABLE folds; a magnitude-based size
  measure for `log` classes would decide them. Driver-gated.
- Which validator change flipped `add_zero`/`mul_one` from UNKNOWN to PROVED was not isolated.
- A Gaussian-integer or Gaussian-rational tier over these pairs is the natural next module: the
  same pair machinery, with `norm = a² + b²` where the integer tier's facts enter.

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/rational-tier-poc/build_records.py            # mint + gauntlet (needs z3)
python3 tooling/commons-node/publish_records.py evolution/rational-tier-poc/records --sign <seed>
```

No credential or network is needed for the builder; the publish step needs a node.
