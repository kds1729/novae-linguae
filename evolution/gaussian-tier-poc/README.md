# A Gaussian-integer tier: nonlinear contracts end to end, and the ground rung the Euclidean property needed

- **Status:** published (2026-10-30). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-30, last updated 2026-10-31 (round 4 — the measured descent)
- **Scope:** `tooling/validator` (the prover's ladder — a new ground rung; the contract set; closed
  laws; the refinement checker's reporting; round 4: the termination checker's measured descent and
  the induction rung over a callee measure), `spec/evaluation.md`.
- **Provenance:** upstream commit `0307132` (nl-validator 0.1.6 in-tree, after the ground rung) for
  rounds 1–3; round 4 on the measured-descent commit (nl-validator 0.1.7 in-tree, see git log);
  z3 4.13.3 (Debian/Ubuntu package); python 3.14 for the builder, whose reference values are
  plain Python integers; one laptop (Ubuntu 26.04). Nothing applied from another module: the
  tier is self-contained, and its question is the solver's, not the linker's.
- **Resolution:** —

## Summary

Fourteen pure records over `(int, int)` pairs read as `a + b·i` — add, sub, neg, conj, mul, the
norm `a² + b²`, the unit test, nearest-integer division of ints, Gaussian division and remainder,
Euclid's algorithm on the norm, and three checker records — were authored in surface syntax and
put through the gauntlet four times, the validator changing between rounds. **Round 4: 14/14
run and certify; termination SOUND on all 14; 19 of 19 contracts SOUND; laws 25 PROVED / 0 UNKNOWN
/ 0 UNSUPPORTED of 25.** (Round 3 stood at termination 13/14 and laws 23 / 0 / 1 of 24.)
Every fact here is nonlinear — commutativity and associativity of the product (degree 2 and 3,
six variables), the norm non-negative and zero only at zero, the norm multiplicative (the
Brahmagupta–Fibonacci identity, degree 4), conjugation multiplicative, units on the axes
(`a² + b² = 1 ⇒ a = 0 ∨ b = 0`) — and the solver decides each as a polynomial problem. The
headline theorem is the **Euclidean property**: the remainder `z − q·w` of the nearest-integer
division has a smaller norm than `w`, stated as a post on `gi_rem` that applies `gi_norm` by
address (`N(result) < N(c, d)`), and SOUND; the checker law that composes `gi_rem` and `gi_norm`
is then PROVED from it. Through round 3 one law stayed UNSUPPORTED — `gi_gcd`'s right zero: the
record is recursive with a tuple result, and its termination was UNVERIFIABLE, because Euclid's
algorithm on Gaussian integers descends by a *callee's* post (the remainder's norm is smaller), a
measure the termination checker had no vocabulary for. **Round 4 (2026-10-31) is that rung — the
measured descent:** over the linked view the checker reads, off the contracts it re-derives, a
callee whose PROVED post has the shape `M(fst(result), snd(result)) < M(p_i, p_j)` with `M` a
terminating callee whose result is PROVED bounded below, and takes the components `case rem(z, w)
of (r1, r2) => self(w, (r1, r2))` as a well-founded descent of the positions `M` is taken at — once
the solver has discharged `rem`'s precondition (`w ≠ 0`) from the branch facts in force (the false
arm of `c = 0 ∧ d = 0`). The prover's induction rung inducts on the same measure: `right_zero` is
an unfolding (its measure arguments are literals), and the new law `nonzero` — the gcd is nonzero
unless both inputs are — is a genuine strong induction on `N(c, d)`, the hypothesis at `(c, d,
rem(z, w))` ordered by `N(rem) < N(w)` from the remainder's contract. Termination SOUND 14/14, laws
25/25.

The property took **one prover rung and three validator fixes**, all in-tree. The rung (`0307132`,
the **ground rung**, `tooling/validator/src/ground.rs`): the remainder's post, with `gi_div`'s four
rounding bounds, `gi_mul`'s exact posts and `gi_norm`'s definition stated at the body's
applications, is a degree-4 polynomial problem that z3 decides in under a second as plain
`QF_NIA` — and does not decide in 90 s with the callees as uninterpreted functions under
`(set-logic ALL)`. The rung rewrites the instances-only certificate: `let`s expanded, `define-fun`
applications beta-reduced, tuple projections of constructors reduced, equalities between tuples
split into projections, every remaining callee application a fresh constant per distinct term,
quantified asserts dropped, logic `QF_NIA`. Ackermann's reduction without the congruence
constraints, weaker than the certificate, so its unsat is the certificate's. The fixes (`0af6cd1`):
a closed law (`i² = −1`, written without `forall`) read UNSUPPORTED "not a `forall`" — it is an
obligation with no variables; a record's **own** posts and laws may apply a callee its body does
not (`gi_norm`'s law `N(conj z) = N(z)` applies `gi_conj`; `gi_rem`'s post applies `gi_norm`), and
their contract set came from the body's callees — the fourth place that assumed a contract
applies no callee, after the rational tier's three; and the refinement checker reported the list
engine's "nothing to induct on" where the first-order refusal was the informative one.

## Provenance of the inputs

| input | pinned at |
|---|---|
| `nl-validator` | 0.1.6 in-tree, commit `0307132`; the laws proved with z3 4.13.3 |
| reference values | Python integers: `mul`, `norm`, `round_div(n, d) = (2n + d) // (2d)`, `div`, `rem`, `gcd` in `build_records.py`, every worked example recomputable |
| the builder | `build_records.py` here — re-mints deterministically (`--parse-only` mints without the gauntlet); rerun after any validator change |

## What was measured

Builder round 3 (`records/REPORT.{json,md}`; the log in the author's sandbox), 1.2 min, z3 4.13.3
(the bodies are all `O(1)` polynomials; the rational tier's gcd-bearing rounds took 30–56 min):

| record | signature | contracts | laws (prove) | generative |
|---|---|---|---|---|
| `gi_add` | 4 ints → `(int, int)` | both components exact, SOUND | `commutative`, `associative` (6 vars), `zero_identity` **PROVED** | HELD 400; HELD 400; EXHAUSTIVE 81 |
| `gi_sub` | | exact ×2 SOUND | `self_difference_is_zero` **PROVED** | EXHAUSTIVE 81 |
| `gi_neg` | 2 ints → pair | exact ×2 SOUND | `involution` **PROVED** | EXHAUSTIVE 81 |
| `gi_conj` | | `fst = a`, `snd = −b` SOUND | `involution` **PROVED** | EXHAUSTIVE 81 |
| `gi_mul` | 4 ints → pair | `fst = ac − bd`, `snd = ad + bc` SOUND | `commutative`, `one_identity`, `i_squared_is_minus_one` (closed; round 1: UNSUPPORTED), `zero_annihilates`, `associative` (degree 3) **PROVED** | HELD 400; EXHAUSTIVE 81; UNGENERATABLE (closed); EXHAUSTIVE 81; HELD 400 |
| `gi_norm` | 2 ints → int | `result ≥ 0`, `result = a² + b²` SOUND | `zero_only_at_zero`, `positive_off_zero`, `conj_invariant` (applies `gi_conj`; round 1: UNSUPPORTED) **PROVED** | EXHAUSTIVE 81 ×3 |
| `gi_norm_mul_holds` | checker | — | `norm_multiplicative` (degree 4) **PROVED** | HELD 400 |
| `gi_conj_mul_holds` | checker | — | `conj_multiplicative` **PROVED** | HELD 400 |
| `gi_is_unit` | 2 ints → bool | — | `units_lie_on_an_axis`, `the_four_units` (closed) **PROVED** | EXHAUSTIVE 81; UNGENERATABLE |
| `gi_round_div` | `n d → int`, pre `d ≥ 1` | `−d ≤ 2(n − q·d) < d` (two posts) SOUND | `exact_on_multiples`, `by_one` **PROVED** | EXHAUSTIVE 81, 9 |
| `gi_div` | 4 ints → pair, pre `w ≠ 0` | the rounding bounds on both components (four posts, through the `let`-bound norm) SOUND | `by_one` **PROVED** | EXHAUSTIVE 81 |
| `gi_rem` | 4 ints → pair, pre `w ≠ 0` | **`N(result) < N(w)` SOUND** (rounds 1–2: UNVERIFIABLE) | `by_one_is_zero` **PROVED** | EXHAUSTIVE 81 |
| `gi_euclid_holds` | checker | — | `remainder_smaller` **PROVED** (rounds 1–2: UNKNOWN) | HELD 400 |
| `gi_gcd` | 4 ints → pair, recursive; termination **SOUND** on `N(c, d)` by `gi_rem`'s post (rounds 1–3: UNVERIFIABLE) | — | `right_zero` **PROVED** by unfolding (rounds 1–3: UNSUPPORTED, no established measure); `nonzero` **PROVED** by strong induction on `N(c, d)` (round 4) | EXHAUSTIVE 81; HELD 400 |

Totals (round 4): 14/14 run PASS, 14/14 certified, termination SOUND 14 / UNVERIFIABLE 0 (round 3:
13 / 1, `gi_gcd`), complexity SOUND 8 / UNVERIFIABLE 6 (the records that pass a product to a callee
— `gi_div`, `gi_rem`, the three checkers — and the recursive `gi_gcd`, whose `O(log n)` is a claim
about the norm's halving no structural analysis states), contracts 19 SOUND / 0, laws 25 PROVED /
0 UNKNOWN / 0 UNSUPPORTED (round 3: 23 / 0 / 1 of 24), 23/23 generatable laws held on the bounded
domain with no skipped cases (the two closed laws are UNGENERATABLE — "not a forall, no domain to
sample" — and PROVED; `nonzero` HELD 400). Round 3's wide pass:
22/22 held under `--wide` (200 cases across magnitudes), where the skipped-case report earns its
keep: `gi_mul`'s associativity held on 129 cases with 71 skipped and `norm_multiplicative` on 61
with 139 skipped, every skip an integer overflow of the evaluator's 128 bits at degree 3–4 over
inputs near 10¹³–10¹⁶, named with the inputs.

**The rounds.** Round 1 (0.2 min): 14/14 certified; laws 19 PROVED / 1 UNKNOWN
(`remainder_smaller`) / 4 UNSUPPORTED (the two closed laws, `conj_invariant`, `right_zero`);
`gi_rem`'s post UNVERIFIABLE with the message "no list-typed quantified variable to induct on" —
which was the fallback's message, not the cause: the first-order path had refused because
`gi_norm`'s contract was not in `gi_rem`'s set. Round 2 (the three fixes): the closed laws and
`conj_invariant` PROVED; `gi_rem`'s post reached the solver and timed out — "solver could not
decide" — as did the checker. Round 3 (the ground rung): everything but `gi_gcd`. Round 4 (the
measured descent, 1.4 min, log `gaussian-build-round4.log`): `gi_gcd` termination SOUND, both its
laws PROVED — the table above.

**The Euclidean property, by hand and by the rung.** Let `N = c² + d²`, `q = (q1, q2)` with
`gi_div`'s bounds `−N ≤ 2·r1 < N` and `−N ≤ 2·r2 < N` for `r1 = (ac + bd) − q1·N`, `r2 = (bc − ad)
− q2·N`, and `m = q·w` with `gi_mul`'s exact posts. Then `(z − m)·conj(w) = (r1, r2)`, so
`N(z − m)·N = r1² + r2² ≤ N²/2`, so `N(z − m) ≤ N/2 < N` for `N ≥ 1`. As `QF_NIA` over the
instances — a hand-written script with the same facts (`~/sandbox`-scratch `euclid_hand.smt2`) —
z3 answers unsat at once; the prover's own instances-only certificate, Ackermannized by a
prototype (`~/sandbox/ackermannize.py`), likewise; the same certificate with `callee_…` symbols
and the `dt0` datatype under `(set-logic ALL)`: unknown at 5 s and at 90 s. The rung is that
rewrite, in the ladder after the instances-only retry and before the Euclidean rung, at the
default budget.

## What is argued

- **Nonlinear contracts compose like linear ones once the solver sees arithmetic.** Every post
  and law here is a polynomial statement, and all 19 posts and 23 laws closed — through contracts
  (`gi_div`'s bounds into `gi_rem`'s post into the checker's law), not by unfolding. What stood in
  the way was not the mathematics but the query's shape: uninterpreted symbols for the callees
  and a datatype for the pairs kept z3 in the combination solver. The ground rung's rewrite is
  trivial logically (drop congruence, drop quantifiers, name the terms) and decisive in practice.
- **The rewrite is a proof rung because it is weaker.** A model of the certificate evaluates to a
  model of the rewritten formula, so the rewrite's unsat is the certificate's unsat; the converse
  fails, so a sat means nothing and is never reported. The same discipline as every other rung
  here: never a false SOUND.
- **"A contract applies a callee" has now been checked in four places.** The rational tier found
  three (instance scope, declaration order, the closure over callees' contracts); this module
  found the fourth (the record's own predicates' callees) because its contracts apply callees the
  body does not. Each place was an assumption no test had named; each is now a test.
- **Termination by a callee's post was the next rung, and it is built (round 4).** `gi_gcd` recurses
  on `(w, rem(z, w))` and terminates because `N(rem(z, w)) < N(w)` — a fact that IS a certified post
  on `gi_rem`, over a measure that is a certified function (`gi_norm`) of two arguments. The
  checker's vocabulary was single-argument descent (structural `tail`, guarded decrement, halving, a
  callee's size bound on one position); a measure that is a callee of several positions, decreasing
  by another callee's post, is the generalization — and it is sound only with the precondition
  discharged, since a post is certified under its pre. The checker therefore carries the boolean
  branch conditions down the walk and asks the solver whether they entail the pre at the call;
  the structural checker gained one solver query, at the one place a certified fact enters it.
  The same measure drives the induction rung, whose hypothesis is ordered by the measure callee's
  value (`N(c', d') < N(c, d)`) — well-founded because `N ≥ 0` is certified. Two things had to be
  true that no test had named: a recursive call whose arguments are a pattern's components
  (`self c d r1 r2`) must lower in parameter scope (the components are resolved to `fst(rem(…))`,
  `snd(rem(…))` — before, such a call refused the whole certificate), and the measure callee's
  contract must be installed for the hypothesis to mention it (the contract closure already brings
  it: `rem`'s post names it). Measured on the record: termination SOUND, `right_zero` PROVED by
  unfolding, `nonzero` PROVED by the quantified hypothesis in one solver call under the default
  budget (18 s wall for the whole `prove`, contract derivation included).
- **Degree and magnitude set the wide sampler's reach.** Products of four 10¹⁶-scale integers
  overflow 128 bits; the sampler says so per case and counts only what it evaluated. The two
  skip-heavy laws are the two PROVED ones the sampler reaches least — the right division of labor.

## What worked well

- The whole module runs in about a minute: polynomial bodies are the solver's home ground once
  the query is in the right logic, and the gauntlet's three rounds cost less than one rational
  round.
- Capturing the solver's queries (the shim) and re-running them by hand isolated the cause in a
  few minutes: same facts, two logics, two answers. The prototype Ackermannizer written to test
  the hypothesis became the specification of the Rust rung.
- Closed laws (`i² = −1`, the four units) are the simplest statements in the module and were the
  first to be refused; both the prover and the generative checker had assumed every law is a
  `forall`. The prover is fixed; the checker still reports UNGENERATABLE — recorded.

## Still open

- ~~Termination through a callee's post~~ — CLOSED in round 4 (the measured descent; above).
  Its residue: the scalar form `M(result) < M(p)` is recognized only for a callee applied directly
  in the self-call's argument, and a measure of THREE or more positions needs a tuple pattern with
  that many binds (the pair form is what the Gaussian tier needed; `fst`/`snd` are the projections
  the predicate grammar has).
- **The hypothesis as instances for a callee measure.** `nonzero` closed on the quantified
  hypothesis; the instances form (at the recursive call, under `N(rem) < N(w)`) is emitted too but
  was not what closed it. A law that needs the instances would measure that path.
- **Closed laws in the generative checker**: UNGENERATABLE where one evaluation would do. Small.
- **Complexity through product arguments** (six UNVERIFIABLE folds), as in the rational tier.
- A Gaussian-rational tier (pairs of these over the rational tier's normalization) and Gaussian
  primes (`N(z)` prime, or `z` a rational prime `≡ 3 mod 4`) would reuse all of this.

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/gaussian-tier-poc/build_records.py            # mint + gauntlet (needs z3; ~1 min)
python3 tooling/commons-node/publish_records.py evolution/gaussian-tier-poc/records --sign <seed>
```

No credential or network is needed for the builder; the publish step needs a node.
