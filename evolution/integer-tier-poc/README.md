# A certified integer tier: number theory and the NTT as in-language records, and what the gauntlet found

- **Status:** absorbed (2026-10-17; the four open questions closed the same day, below). Every
  language-level finding below is in the tree: eight evaluator/checker defects fixed, two checkers
  widened, and — from the follow-up — callee folding over a linked view, a numeric-induction rung
  in the prover, variable divisors, and magnitude-wide sampling (`tooling/validator`); the records
  published to the commons with signed certifications. The 41 records and their builder live here
  (`records/`, `build_records.py`) — reproducible with no credential. Final tally (fourth follow-up):
  41/41 certify, termination AND complexity SOUND on all 41 (the NTT chain at `O(n log n)`, with no
  fuel parameter), 18 contracts and 13 laws proved — `lcm`'s through `gcd`'s certified contract,
  `ext_gcd`'s Bézout identity over a tuple result — and 31 laws held both over the bounded domain and
  across magnitudes.
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-17, last updated 2026-10-17
- **Scope:** `tooling/validator` (`interp.rs` arithmetic + comparison + recursion depth,
  `proptest.rs`, `eval.rs`, `prove.rs`, `induct.rs`, `terminate.rs`, `complexity.rs`, `main.rs`),
  `spec/evaluation.md`; read against `spec/surface-syntax.md` §2 (predicate surface) and
  `spec/intent-tag-vocabulary.md` (`math/<…>`).
- **Provenance:** upstream commit `b0c7183` (the tree before this module); `nl-validator` built
  with rustc 1.99 (release profile); z3 4.13.3 (Debian/Ubuntu package); python 3.14 for the
  builder; one laptop (Ubuntu 26.04). Reference values for the worked examples computed with
  Python's integers (`pow`, `math.isqrt`, a 12-base Miller–Rabin, a textbook NTT) inside the
  builder, so every expected value is recomputable.
- **Resolution:** absorbed — the fixes and widenings travel in their own commit, the module in
  this one. No proposals: the one design question (composition is opaque to the termination and
  complexity checkers) is recorded under *Open questions* with a recommendation.

## Summary

Thirty-eight pure records — Euclid and Bézout, modular arithmetic and exponentiation, modular
inverse and a two-modulus Chinese remainder, integer square root, a deterministic Miller–Rabin for
n < 2^63 beside trial division, digit expansions, binomial coefficients, and a number-theoretic
transform over Z/998244353Z with inverse and convolution (the FFT's integer cousin) — were authored
in surface syntax, composed by content address, and put through the verified-by-default gauntlet.
All 38 run their worked examples (up to 2^61 − 1, the largest prime below 2^63, a 30-digit binomial,
a strong pseudoprime that fools the first nine bases) and all 38 certify. Twenty-eight algebraic
laws decide generatively, 26 exhaustively over the bounded domain; four laws prove over the
unbounded domain; two modular postconditions prove SOUND. The uncomfortable part is the first run:
before this module the gauntlet could decide **none** of the laws (`self(a, b)` was unreadable to
the runtime evaluator and the prover rejected every surface-authored literal), **2^100 squared
evaluated to 0** (silent 128-bit wrap), **2^100 − 1 < 2^100 was false** (int comparison through a
float), a 56,000-step recursion **aborted the process** (stack overflow, not a refusal), and the
generator refuted a true law about `gcd` by sampling negative ints into `nat` parameters. All of it
is fixed in-tree, each fix pinned by a test, and the termination/complexity checkers now read
Euclid's `mod` step and halving descents — 10 of the 35 original records moved from UNVERIFIABLE to
SOUND on termination. What remains UNVERIFIABLE is structural: composition by `fn_ref` is opaque to
both checkers, and the first-order prover proves only non-recursive laws (the number theory worth
proving is recursive).

## Provenance of the inputs

| Input | Pinned at |
|---|---|
| Repository | `b0c7183` before; this module's commit and the validator commit after |
| Validator | `cargo build --release`, rustc 1.99; 609 tests after the changes (600 before) |
| Solver | z3 4.13.3 (apt); `prove` default |
| NTT prime | p = 998244353 = 119·2^23 + 1, primitive root 3 (lengths up to 2^23 divide p − 1) |
| Miller–Rabin bases | 2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37 — deterministic for n < 3.3·10^24 (Sorenson–Webster 2015); the records declare n ≤ 2^63 − 1 so x·x stays inside i128 |
| Reference values | Python 3.14 integers inside `build_records.py` |

## What was measured

Every number below comes from `build_records.py` (its stdout is the log; `records/REPORT.json`
and `records/REPORT.md` are the per-record tables). `REPORT-before-fixes.md` is the table from the
first run, on the validator as it stood at `b0c7183`.

**The records.** 38 pure records in surface syntax: 23 number-theoretic (`gcd`, `lcm`, `ext_gcd`,
`mod_inverse`, `mod_add`, `mod_mul`, `is_even`, `pow_mod_naive`, `mod_pow`, `isqrt`,
`mr_squarings`, `mr_witness`, `odd_part`, `mr_all`, `is_prime`, `no_divisor_from`,
`is_prime_trial`, `crt2`, `digits`, `from_digits`, `digit_sum`, `binomial`, `next_pow2`), 8 for the
transform (`ntt_split`, `ntt_butterfly`, `ntt_rec`, `ntt`, `ntt_inverse`, `pointwise_mod`,
`pad_to`, `convolution`), 7 **checker records** (`bezout_holds`, `inverse_holds`, `pow_mod_agree`,
`prime_agree`, `crt_holds`, `digits_roundtrip`, `ntt_roundtrip`: bool-valued compositions carrying
the law `forall … self(…) == true`, because a predicate cannot project a tuple, match a `Maybe`, or
name a second record). 14 records apply others by `fn_ref`. Record size: 432 KB for records +
bodies.

**Run (worked examples): 38/38 PASS**, including `mod_pow(2, 2^62, 2^61 − 1)`, `isqrt(2^100)` and
`isqrt(2^100 − 1)`, `binomial(100, 50)` = 100891344545564193334812497256, `is_prime` on 561 and
1105 (Carmichael), 2047, 1373653, 25326001, 3215031751 (strong pseudoprimes to the first bases),
3825123056546413051 (a strong pseudoprime to bases 2–23, caught by 29), 2^61 − 1 and
9223372036854775783 (primes), and the NTT on lengths 1, 4, 8 with inverse and a 3×3 convolution.

**Certify: 38/38 CERTIFIED** (the conservative UNVERIFIABLE verdicts do not revoke; nothing fails
its declaration). Per check, before the fixes (35 records) → after (38):

| check | before | after |
|---|---|---|
| termination | 8 SOUND / 27 UNVERIFIABLE | 18 SOUND / 20 UNVERIFIABLE (18 of the 20: a `fn_ref` callee is opaque; 2: `digits`/`digit_sum` divide by a *variable* base) |
| complexity | 11 SOUND / 24 UNVERIFIABLE | 17 SOUND / 21 UNVERIFIABLE (18 opaque; `digits`, `digit_sum` as above; `pad_to` descends two parameters) |
| refinements | 0 SOUND | 2 SOUND (`mod_add`/`mod_mul`: `0 ≤ result < m` under `m > 0`, by SMT); 10 UNVERIFIABLE — all on recursive bodies (`self` is outside the first-order fragment); 27 N/A |
| laws, generative | 0 of 25 decidable (every one UNGENERATABLE) | 28 of 28 decided: 26 EXHAUSTIVE over the bounded domain (ints −4..4, nats 0..8), 2 HELD over 400 samples |
| laws, prove | 0 (25 UNSUPPORTED, misreported) | 4 PROVED (`is_even` period and alternation; `mod_add`/`mod_mul` commutativity), 2 UNKNOWN (the congruence laws — `mod` by a variable is nonlinear for z3), 22 UNSUPPORTED (14 recursive bodies, 8 composed heads) |

The termination moves: `gcd` and `ext_gcd` (Euclid's `mod` descent), `mod_pow`, `odd_part`,
`isqrt`, `next_pow2` (halving and ceiling-halving descents), `no_divisor_from` (the false arm of
`d < 2` as the floor). The complexity moves: `gcd`, `ext_gcd` → `O(log n)` (Lamé), `next_pow2`.

**Evaluator defects, each measured then fixed:**

1. **Silent 128-bit wrap.** `mul(2^100, 2^100)` evaluated to `0`; the arithmetic was plain `x * y`.
   It also produced a *false CONTRADICTED* on `isqrt`'s law `self(n·n) == n` at the worked example
   n = 2^100. Now every int result that does not fit refuses (`integer overflow`).
2. **Comparison through a float.** `lt`, `le`, `gt`, `ge` converted both operands to f64 always:
   `2^100 − 1 < 2^100` was **false**. This is where guards live. It produced a false CONTRADICTED on
   `isqrt`'s floor law at n = 2^100 − 1. Now two ints compare exactly; the float path remains for
   mixed operands.
3. **Stack overflow instead of a refusal.** The evaluator is a tree walker; trial division on
   3215031751 (56,701 steps) aborted the binary, and so did a 5,000-step recursion — the default
   8 MiB main-thread stack. The CLI now runs on a 2 GiB-stack thread and refuses past 100,000 nested
   applications with a stated reason; measured: 99,000 deep succeeds in 0.63 s at ~7 KB per level.
4. **`self(args)` undecidable at run time.** `parse-predicate` emits `{"op":"self"}` for the natural
   spelling; the runtime/generative evaluator resolved ops as builtins and knew only the older
   `apply(self, …)` form — every law here was UNGENERATABLE. Fixed.
5. **The generator erased `nat`.** It sampled ints into `nat`-typed `self` parameters and REFUTED
   `gcd`'s commutativity at (−2, −1) — Euclid with a Euclidean `mod` is not symmetric on negative
   ints, and that pair is outside the declared domain. Variables fed to `nat` positions now range
   over nats (exhaustive 0..8, sampled 0..64). The same law, over nats: EXHAUSTIVE, 81 cases.
6. **`check-properties` could not link.** No `--records`, so a checker record applying the
   vocabulary by `fn_ref` was undecidable. Flag added.
7. **The prover rejected surface literals.** `unsupported literal kind: None` on every law written
   through `parse-predicate` (bare `0` where the prover expected `{"kind":"int","value":0}`; the
   schema admits both). Both spellings are read now; this is what let `is_even` prove and the
   modular postconditions discharge.
8. **A misreport.** A law about a recursive body failed with "`self` used but no body supplied"
   when a body WAS supplied; the real reason — recursion is outside the first-order fragment — is
   now stated.

**A law corrected by the generator.** `inverse_holds` was first written as `(a·x) mod m == 1`;
the exhaustive pass refuted it at (a, m) = (0, 1): in Z/1Z every residue is 0, so the congruence
`a·x ≡ 1 (mod m)` reads `(a·x) mod m == 1 mod m`. The record carries the corrected law.

**Where the proofs stop.** Four laws prove; the two that do not (`(a + b) mod m` and
`(a · b) mod m` congruences) are nonlinear in the modulus and z3 answers unknown. Every recursive
law — `gcd` commutativity, Pascal's rule, Bézout, the square-root bounds — is UNSUPPORTED: the
first-order prover inlines non-recursive bodies only, and the inductive backend inducts over lists.
Numeric induction exists today only for domain-qualified *equivalence* claims with a constant
descent, so `mod_pow ≡ pow_mod_naive` (halving vs unit descent, arity 3) is out of its fragment too.

**Composition is opaque.** 18 records are UNVERIFIABLE on termination and complexity for one reason:
they apply other records by `fn_ref`, and the checkers treat an applied address as an opaque
callee. `certify --records` folds callees' declared *effects* only.

**Published and used.** 114 artifacts to Arca (38 records, 38 bodies, 38 signed certifications;
0 rejected). Then the agent loop, from the node: `orchestrate --node … --verify
--require-certified --intent math/number-theory/is-prime` on 9223372036854775783 — query by tag →
1 match; 6 records + 6 linked artifacts fetched and hash-verified (the composition closure:
`is_prime` → `odd_part`, `mr_all` → `mr_witness` → `mod_pow`, `mr_squarings`); certified; applied →
`true`; re-verified → **CONFIRMED**; the signed `assert` published (`msg_dcd7cb77…`).

## What is argued

- **The tier is certifiable where the design said it would be, and nowhere else.** Everything here
  is in the Int fragment, pure, total on its declared domain, and the certification gauntlet
  accepted all of it; the float-domain guard would have refused an FFT, and this module did not
  try one. The honest claim stays: verified building blocks for an agent, not a contribution to any
  open problem.
- **The gauntlet was the instrument, and the bugs it found are not number-theoretic.** A silent
  wrap, a float comparison in the guard path, a process abort on deep recursion, and three ways the
  property machinery could not read what the surface parser writes — every one of them a soundness
  or honesty defect in the evaluator that only values past 2^53 and laws written in surface syntax
  could reach. The ecosystem thread never saw them because cloud control planes do not hand back
  2^100.
- **Generative checking over a tiny domain is a precision instrument, not a proof.** Exhaustive
  over 0..8 found two real mistakes (the generator's own `nat` erasure and the Z/1Z edge of the
  inverse law) and would not find a wrong Miller–Rabin base set; the worked examples carry that
  evidence (the Carmichael and strong-pseudoprime rows). Both rungs are needed and neither
  substitutes for a proof.
- **Checker records are the idiom for laws predicates cannot say.** A bool-valued composition with
  the law `forall … self(…) == true` moves tuple projection, `Maybe` matching, and cross-record
  reference into the body, where the language is complete. It costs an extra record per law and
  makes the law opaque to the prover (the body applies a `fn_ref`); it is the right shape until
  predicates can apply records by address at run time — which the predicate evaluator already does
  for claims, so the gap is in the law-checking entry points, not the grammar.
- **Mod-descent and halving are structural measures.** Euclid's step and `div(p, c)` are the two
  recursion shapes number theory is made of; both are well-founded under a positive floor, and the
  floor is exactly what a `case` guard establishes. Reading the FALSE arm of `p < lit` was the
  missing half — the natural way to write a base case.

## What worked well

Surface syntax for bodies and laws (38 records authored as strings, parsed and hashed by the
validator — no hand-written ASTs); composition by content address across the whole tier (the
transform is four levels deep); worked examples as executable tests with values past 2^100
(`"value"` as a decimal string, per the schema); `certify --json` as one re-checkable verdict per
record; exhaustive bounded checking as the first thing a law meets; and the evaluator's refusals
once they were refusals — every failure in the final run is a stated reason.

## Defects reported

The eight evaluator/checker defects above, all fixed in the validator commit that accompanies this
module (each with a unit test): `interp.rs` (checked arithmetic, exact int comparison, apply-depth
bound, `self` op in predicates), `main.rs` (2 GiB stack thread, `check-properties --records`),
`proptest.rs` (`Nat` kind), `prove.rs` + `induct.rs` (bare literals, recursion message).

## Follow-up (2026-10-17, same day): the four open questions, closed

Each was built as recommended, measured on these 38 records, and is in the tree with tests.

### Measured

| check | first run (35 records) | after the fixes (38) | after the follow-up (38) |
|---|---|---|---|
| termination | 8 SOUND / 27 UNVERIFIABLE | 18 / 20 | **33 / 5** (the five: `ntt_rec` recurses on the halves `ntt_split` returns — not a descent of its own parameter — and `ntt`, `ntt_inverse`, `ntt_roundtrip`, `convolution` inherit it) |
| complexity | 11 / 24 | 17 / 21 | **27 / 11** (the primality chain: `mr_witness` passes `mod_pow`'s RESULT to `mr_squarings`, a value the checker cannot bound by the input, and `mr_all`/`is_prime`/`is_prime_trial`/`prime_agree` inherit; `digits_roundtrip` feeds `digits`' result to `from_digits`; the NTT chain as above) |
| refinements | 0 SOUND | 2 SOUND / 10 UNVERIFIABLE | **8 SOUND / 4 UNVERIFIABLE** — proved by numeric induction: `gcd ≥ 0`, `isqrt ≥ 0` and `isqrt² ≤ n`, `0 ≤ mod_pow < m`, `0 ≤ pow_mod_naive < m`, `next_pow2 ≥ 0`; still open: `lcm ≥ 0` (composition), `from_digits ≥ 0` (list recursion with the list in the second position), `digit_sum ≥ 0` and `binomial ≥ 0` (nonlinear steps) |
| laws, generative (bounded) | 0 of 25 decidable | 28 of 28 | 28 of 28 (26 EXHAUSTIVE, 2 HELD) |
| laws, generative (`--wide`, 200 samples to 2^60) | — | — | **28 of 28 HELD**; decided counts drop where the body refuses (depth-bounded trial division at 2^60: `prime_agree` 137/200; the 2^63 modulus bound: `pow_mod_agree` 73/200) |
| laws, prove | 0 | 4 PROVED / 2 UNKNOWN / 22 UNSUPPORTED | **9 PROVED / 4 UNKNOWN / 15 UNSUPPORTED** — new by numeric induction: `gcd(a, 0) = a` (unfolding), `isqrt`'s floor bounds, `binomial(n, 0) = 1`, `next_pow2(n) ≥ n` and `< 2n`; UNKNOWN: `gcd` divides both, the two `mod` congruences, the digit-sum bound (nonlinear steps); UNSUPPORTED: laws applying `self` at two different measures (commutativity, associativity, symmetry, Pascal — a lexicographic/sum measure this rung lacks), the seven checker records (`fn_ref` heads), the two list laws (`length`) |

**Soundness probes of the induction rung.** `count(n) = n`, `count(n) ≥ 0`, `count(n) ≤ n`, and
Gauss `2·Σn = n(n+1)` PROVED; the false `count(n) = 0`, `count(n) = n + 1`, `2·Σn = n²`, and
`gcd(a, b) = a` all UNKNOWN — never REFUTED (SAT is uninformative with an uninterpreted `self`) and
never PROVED.

**Standalone.** `check-termination --records` / `check-complexity --records` on `lcm`: UNVERIFIABLE
without the view (its `gcd` callee is opaque), SOUND with it, O(log n) inferred.

### Argued

- **Fold by re-derivation, never by declaration.** The linked view analyzes each callee's own body
  (transitively, memoized, a cycle unresolved), so a callee re-minted with a body that no longer
  terminates weakens its dependents' verdicts LOUDLY, at check time — the failure mode the open
  question feared is the one this design cannot have. The cost: a callee's complexity class is in
  its own input size, so it folds only when every argument's value is bounded by ours (parameters,
  literals, `add`/`div`/`mod`/`length`/… over them); a recursive result or a product of two inputs
  is refused with the reason. That is exactly where the eleven remaining complexity verdicts sit,
  and they are honest: `mr_squarings` runs `s` steps on a value the checker cannot relate to `n`.
- **Numeric induction is the termination checker's measure, reused.** The induction principle needs
  a well-founded order; the checker already finds it (position + floor) to prove termination, so the
  prover inherits it for free — one analysis, two verdicts. Supplying the unfolding explicitly
  (two levels) and keeping `self` uninterpreted is what makes z3 decide: the same idiom the
  equivalence prover measured out, now general over laws and contracts.
- **What the rung cannot reach is a measure, not a solver.** Commutativity, associativity,
  symmetry and Pascal's rule each apply `self` at two measures; strong induction on one variable
  does not cover them. A lexicographic or sum measure (`a + b`) is the named next rung. The
  nonlinear UNKNOWNs (`binomial ≥ 0`, the `mod` congruences) are the solver's limit, and stay so.
- **Wide sampling is evidence, not proof, and it says so.** 28 of 28 laws held at magnitudes to
  2^60 — including Miller–Rabin vs trial division to the depth bound — which the −4..4 domain
  could never show; the HELD counts under 200 are the body's own refusals (depth, the 2^63 bound)
  skipped, never counterexamples.

### Still open after the first follow-up (closed in the second, below)

- A lexicographic / sum measure for laws that apply `self` at two measures.
- Folding a callee's class when the argument is a *bounded* recursive result (`mod_pow < m`): the
  contract `0 ≤ result < m` is proved now, so the checker could, in principle, read a proved
  postcondition as a size bound — a second use of certified facts, out of scope here.
- `ntt_rec`: recursion on a callee's result (`ntt_split` halves the list) is well-founded only through
  the callee's output-size contract (`preserving`/halving) — the same fact-reuse.

## Second follow-up (2026-10-17, same day): the three remaining rungs

### Measured

41 records now (three helpers added: `log2_floor`, `scale_mod`, and `odd_part` split into
`odd_part_d`/`odd_part_s`; see below why). **41/41 run, 41/41 certify.**

| check | after the first follow-up (38) | now (41) |
|---|---|---|
| termination | 33 / 5 | **41 / 0** |
| complexity | 27 / 11 | **36 / 5** — the NTT chain only: `ntt_rec` applies `ntt_butterfly` to its own recursive results (nothing bounds their size), and `ntt`, `ntt_inverse`, `ntt_roundtrip`, `convolution` inherit |
| refinements | 8 SOUND / 4 UNVERIFIABLE | **14 SOUND / 4** — new: `isqrt ≤ n`, `odd_part_d ≤ d`, `odd_part_s ≤ d`, plus the earlier six (`mod_add`/`mod_mul` posts, `gcd ≥ 0`, …); still open: `lcm ≥ 0` (composition), `from_digits ≥ 0` (list in the second position), `digit_sum ≥ 0` and `binomial ≥ 0` (nonlinear steps) |
| laws, generative | 28 of 28 | **31 of 31** (28 EXHAUSTIVE, 3 HELD); `--wide` **31 of 31 HELD** |
| laws, prove | 9 PROVED / 4 UNKNOWN / 15 UNSUPPORTED | **12 PROVED / 7 UNKNOWN / 12 UNSUPPORTED** — new: **`gcd(a, b) = gcd(b, a)`** (by unfolding alone), `odd_part_d` odd-or-zero, `log2_floor`'s bound; UNKNOWN now includes the two-measure laws that reach the solver (`gcd` associativity, `binomial` symmetry and Pascal — nonlinear steps); UNSUPPORTED: the 7 checker records and `lcm` (`fn_ref` heads), `isqrt(n·n) = n` (its only measure argument is compound and `n` is not nat-constrained), the two list-length laws |

**The ladder decides commutativity.** With the quantified induction hypothesis present z3 answered
*unknown* on `gcd(a, b) = gcd(b, a)` (in 2.3 s, not a timeout); the same certificate **without** the
hypothesis is *unsat*. The defining equation instantiated at `(a, b)`, `(b, a)` and the two recursive
calls inside them already contains the case analysis (`a < b ⇒ a mod b = a`, `a = b ⇒ a mod a = 0`,
`b = 0`), and the hypothesis only obscured it. So the rung tries unfolding alone first, then the
hypothesis. The false probes (`count(n) = 0`, `count(a) = count(b)`, `gcd(a, b) = a`) stay UNKNOWN.

**Proved postconditions as size bounds.** `mr_witness` passes `mod_pow`'s result to `mr_squarings`;
`mod_pow`'s `0 ≤ result < m` is proved (by numeric induction), so the complexity fold reads the result
as bounded by `n` — `mr_witness` SOUND, and up the chain. `odd_part` returned a tuple `(d, s)`, and a
tuple component carries no provable bound (predicates cannot project tuples): split into two scalar
records with `result ≤ d` each, both proved, and `is_prime`'s complexity folds (`O(n²)` in the
checker's measure: the list of twelve bases times a witness test it reads as linear). `isqrt` gained
the second post `result ≤ n` — the size bound `is_prime_trial` needed. `digits` as a list result:
the output-size analysis now reads a halving recursion consing O(1) per level as Linear, so
`from_digits(digits(b, n))` folds.

**The NTT recursion.** No certified fact says a half of `ntt_split`'s result is shorter than the
input (a tuple of lists; no output-size vocabulary for "strictly smaller"), so the depth is carried
explicitly: `ntt_rec xs w levels` counts a nat down (`levels = log2_floor(length xs)`, supplied by
`ntt`/`ntt_inverse`). Termination SOUND by the nat descent. The checker then reads the two-way
branching on a constant descent as exponential in `levels` — which is `n`, correct and useless —
so the NTT chain's `O(n log n)` stays UNVERIFIABLE on complexity; the halving the recursion rests on
is a fact about `ntt_split` the checker has no vocabulary for. `ntt_inverse`'s `map (\x -> …)` was a
higher-order application (opaque to both checkers); it is now the first-order `scale_mod`.

### Argued

- **An induction hypothesis can be a liability.** Strong induction is the general principle, but a
  quantified hypothesis over nonlinear arithmetic (`mod` by a variable) sends z3 into unknown even
  when the goal needs no induction at all. Trying the hypothesis-free certificate first costs one
  solver call and recovers every unfolding-decidable law; the order is the design.
- **A sum measure is the honest multi-variable principle.** Induction on `a + b` over the nats is
  well-founded with no lexicographic machinery, and the hypothesis then covers every application
  whose measure arguments add up to less; requiring every variable in the sum to be
  nat-constrained is what keeps it sound (an int variable could make the sum unbounded below).
  The two-measure laws it reaches but does not close (associativity, Pascal, symmetry) now fail on
  the solver's nonlinear arithmetic, not on the prover's vocabulary — a different, better residue.
- **Certified facts compose, and tuples are where they stop.** A proved postcondition on a scalar
  result is a size bound the complexity checker can use; a tuple result has no predicate
  vocabulary to state one. Splitting `odd_part` is the operator paying that cost in records rather
  than the language paying it in grammar — the right trade until predicates project tuples.
- **Fuel makes termination certifiable and complexity blind.** An explicit depth parameter is a
  legitimate termination measure, and the honest effect on complexity (exponential in fuel) is a
  true statement about what the checker can see. The remaining NTT complexity verdicts are the one
  place in this tier where the language needs a fact it cannot yet express: a callee's output is
  strictly smaller than its input.

### Still open after the second follow-up (the middle one closed in the third, below)

- Complexity of the NTT chain: a `strictly-shorter` output-size class (or a provable
  `length(fst(result)) < length(xs)` — predicates would need tuple projection) for `ntt_split`.
- `lcm ≥ 0` through composition: a callee's proved `result ≥ 0` could discharge a caller's
  nat-result contract the way a proved upper bound now feeds the complexity fold.
- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, the `mod` congruences): solver
  limits, unchanged.

## Third follow-up (2026-10-17, same day): contracts compose

### Measured

**Callee contracts as axioms.** Over the linked view, a `fn_ref` callee is an uninterpreted function
and its CERTIFIED contract — every postcondition its own `check-refinement` proves, every law its own
`prove` proves, the type-implied `result ≥ 0` of a nat result — is a quantified axiom guarded to the
callee's preconditions and nat domain, re-derived on the same view (the callee's callees first).
`gcd` gained the postcondition `(a < 1 ∧ b < 1) ∨ result ≥ 1` (PROVED by numeric induction). Then:

| | before | now |
|---|---|---|
| refinements | 14 SOUND / 4 UNVERIFIABLE | **16 / 3** — `lcm ≥ 0` from `gcd`'s proved `≥ 1`, plus `gcd`'s new post itself; open: `from_digits`, `digit_sum`, `binomial` (list position / nonlinear) |
| laws, prove | 12 PROVED / 7 UNKNOWN / 12 UNSUPPORTED | **13 / 10 / 8** — `lcm`'s commutativity PROVED from `gcd`'s proved commutativity (no re-proof of `gcd`); the checker records over Int callees (`pow_mod_agree`, `prime_agree`, `lcm` multiple-of-both) now REACH the solver and read UNKNOWN (a contract does not pin a function); UNSUPPORTED is down to callees whose results leave the fragment (tuples, `Maybe`, lists) and the compound-measure `isqrt(n·n) = n` |
| everything else | | unchanged: 41/41 run and certify, termination 41/41, complexity 36/5, 31/31 laws held bounded and wide |

**Two honesty rules the measurement forced.** (1) The first run reported `lcm`'s commutativity
**REFUTED** and `lcm ≥ 0` **VIOLATED**, with a model in which the uninterpreted `gcd` returned −1 —
a spurious counterexample about a function the axioms only partly pin. A certificate that used a
contract now reports a satisfiable negation as UNKNOWN, never REFUTED. (2) `prove` ranged a law over
ALL ints while `gcd`'s axioms are guarded to its nat domain, so `lcm(a, b) = lcm(b, a)` was asked
about negative arguments the record never admits and timed out (60 s, z3); laws now range over the
function's declared domain (variables fed to `nat` parameters are ≥ 0), as the generative checker
already samples them. With both: PROVED in well under a second.

### Argued

- **Certified facts are the composition primitive.** The commons already composes *bodies* by
  content address; this composes *proofs* by content address — `lcm`'s contract rests on `gcd`'s
  address and `gcd`'s proof, both re-derivable by anyone holding the records. Nothing is trusted from
  a declaration: a callee whose contract no longer proves silently contributes no axiom, and the
  caller's verdict weakens honestly. This is the same discipline as the termination/complexity fold,
  applied to the prover.
- **Under-constrained symbols change what SAT means.** The first-order prover's REFUTED is a real
  counterexample only when every symbol is fully pinned; an uninterpreted callee is pinned only by
  its contract. The rule "SAT with a contract ⇒ UNKNOWN" costs precision (a genuine counterexample
  is reported as unknown) and buys the only thing that matters here: no false refutation ever.
- **The declared domain is part of the law.** A record's `nat` parameters are a promise about where
  it is meant to run; a law about it is a claim over that domain. Ranging the prover over all ints
  was not stricter, it was asking a different question — and one the composition axioms could not
  answer.

### Still open after the third follow-up (the first two closed in the fourth, below)

- Complexity of the NTT chain: a `strictly-shorter` output-size class (or a provable
  `length(fst(result)) < length(xs)` — predicates would need tuple projection) for `ntt_split`.
- Contracts for callees whose results leave the Int/Bool/String fragment (`ext_gcd`'s tuple,
  `mod_inverse`'s `Maybe`, the list records): the seven checker records stay UNSUPPORTED until the
  prover's sorts grow (tuples and sums as SMT datatypes).
- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, the `mod` congruences, the
  equivalence checkers `pow_mod_agree`/`prime_agree`): solver limits, or facts no contract states.

## Fourth follow-up (2026-10-17, same day): the design frontiers

### Measured

**Tuples and sums are SMT datatypes.** The prover registers the DECLARED types — the function's
own signature, a callee's from its contract — as per-thread datatypes: tuple construction and
destructuring patterns, variant construction and matching (tester + accessor), and `fst`/`snd`
lower to constructors and accessors; every certificate declares the datatypes it uses. So a
contract can be stated about a tuple result and a checker record can match a callee's `Maybe`:

| | before | now |
|---|---|---|
| `ext_gcd` Bézout post `fst(result) = a·fst(snd(result)) + b·snd(snd(result))` | unstatable (no tuple projection in predicates) | **SOUND** by numeric induction over the datatype |
| checker records over tuple/`Maybe` callees (`bezout_holds`, `inverse_holds`, `crt_holds`) | UNSUPPORTED (callee's sort) | reach the solver: **UNKNOWN** (nonlinear — the contract pins `ext_gcd` by its proved Bézout identity, but `a·x + b·y ≡ … mod m` is outside z3's linear core) |
| refinements | 16 SOUND / 3 UNVERIFIABLE | **18 / 3** — `ext_gcd`'s Bézout post and `next_pow2`'s bound (below); open: `from_digits` (list head), `digit_sum`, `binomial` (nonlinear) |
| laws, prove | 13 PROVED / 10 UNKNOWN / 8 UNSUPPORTED | **13 / 13 / 5** — UNSUPPORTED is down to the compound-measure `isqrt(n·n) = n`, `digits_roundtrip` (a list-result callee) and the three list-length laws (structural, not numeric, induction) |

**Shrinking-callee descents: the NTT chain without fuel.** `ntt_rec` recurses on the halves
`ntt_split` returns — not a descent of its own parameter, so the second follow-up carried an explicit
`levels` counter. Now a recursion on the COMPONENTS of a callee's tuple result is well-founded when
the callee's own body shows each component strictly shorter than its list input (`component_shrink`:
a `tail^k` recursion, k ≥ 2, at most k − 1 conses per component per step, so a component is at most
n − ⌊n/k⌋ long) and the guards in force establish length ≥ k (the FALSE arm of `null(tail xs)`).
Termination reads it as a structural descent, complexity as a halving one. The `levels` parameter is
gone; `ntt_rec` is `\xs w -> …` again.

**The last complexity gap: a callee applied to the function's OWN recursive results.** With the
halves' descent in place, `ntt_rec` still refused: it applies `ntt_butterfly` to `self ev w2` and
`self od w2`, and nothing bounded the size of a recursive result. Four facts close it, each
re-derived from bodies on the linked view: (1) a function's recursive result is size-bounded when
its declared list result is at most linear in its input; (2) the output-size analysis establishes
that for a two-way halving recursion whose per-level material is O(1), where the components of a
**preserving** callee — `ntt_butterfly`'s: a `tail^k` recursion on its list parameters consing at
most k elements per component per step, so each component is no longer than the list it was applied
to — count as the recurrence's carried terms when each is used at most once and bound by a distinct
recursive result (`append lo hi` is `S(n) = 2S(n/2) + O(1) = Θ(n)`; `append lo lo` is refused);
(3) the names a `case` binds from a size-bounded scrutinee are size-bounded (the halves of
`ntt_split xs`); (4) `w·w mod P` is bounded by the literal modulus whatever `w` is. For
`convolution` one more: `pad_to xs (next_pow2 m)` needs `next_pow2`'s result bounded, and its
natural linear bounds `result ≤ 2n + 1` / `result ≤ 2n` do NOT close by induction (the step doubles
a loose hypothesis); `result ≤ max(1, 2n − 1)` is the inductive form (exact at n = 0) and now reads
as an upper bound.

| | before | now |
|---|---|---|
| complexity | 36 SOUND / 5 UNVERIFIABLE (the NTT chain) | **41 / 0** — `ntt_rec` `T(n) = 2T(n/2) + O(n)`, and `ntt`, `ntt_inverse`, `ntt_roundtrip`, `convolution` fold to `O(n log n)` |
| termination | 41 SOUND (with `levels`) | **41 SOUND** (without) |
| everything else | | unchanged: 41/41 run and certify, 28 laws EXHAUSTIVE + 3 HELD, 31/31 HELD wide, no refutation anywhere |

**The nonlinear unknowns are a solver limit, measured.** The Pascal and symmetry induction
certificates (`prove --smt-out`) were re-run under cvc5 1.3.4 (`--tlimit=60000`): both time out exactly
as z3 does at 60 s. Two solvers, same silence — recorded, not pursued.

### Argued

- **Declared types are the sort vocabulary.** The prover never guesses a sort from a body: a tuple
  or sum is an SMT datatype because the SIGNATURE says so (the function's own, or a callee's
  contract), the same source the type checker holds the body to. That is why a contract about a
  tuple result composes exactly like one about an Int — it is the same certified fact, one sort
  wider.
- **Every size fact is re-derived, never declared.** Shrinking components, preserving components,
  a linear own-output, a proved upper bound: each is computed from the callee's body on the linked
  view (memoized, cycle-safe) and feeds the caller's analysis as a fact about that body. A callee
  re-minted with a body that no longer shrinks silently withdraws the descent and its dependents'
  verdicts weaken — the discipline of the first follow-up, applied to sizes.
- **Affine use is the price of carried terms.** Reading a preserving callee's components as the
  recurrence's carried terms is sound only when each is used at most once and bound by a distinct
  recursive result; `append lo lo` would double the material per level and `F(self ev, self ev)`
  would count one half twice. The checker refuses both — the rule is as narrow as the proof.
- **The inductive form of a bound is not its natural form.** `next_pow2 ≤ 2n + 1` is true and
  not inductively provable in one step; `≤ max(1, 2n − 1)` is. A record author who wants the bound
  to compose has to state the invariant the induction can carry, exactly as in a paper proof.

### Still open after the fourth follow-up (the list items closed in the fifth, below)

- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, the `mod` congruences, Bézout
  and CRT as checker laws, `pow_mod_agree`/`prime_agree`): both solvers time out; a lemma rung
  (stating `mod` distributivity as axioms) is the next lever if a driver appears.
- `digits_roundtrip` and the three list-length laws: numeric induction is the wrong principle for
  list-structural recursion; the list-induction backend (`induct.rs`) does not yet read records by
  `fn_ref`. `from_digits ≥ 0`: a `head` in the inductive step.
- `isqrt(n·n) = n`: a compound measure (`self` at `n·n`) — needs the two-measure principle.

## Fifth follow-up (2026-10-17, same day): the list rung

### Measured

The "Still open" list above named five list items. Re-running the gauntlet after the validator
changes below, with every record re-minted (addresses unchanged — bodies and signatures are the
same; certifications re-signed where their content moved):

| law / contract | before | now |
|---|---|---|
| `scale_mod`: `length(self(xs, c)) = length(xs)` | UNSUPPORTED | **PROVED** by structural induction on `xs` |
| `pointwise_mod`: `length(self(xs, ys)) ≤ length(xs)` and `≤ length(ys)` | UNSUPPORTED | **PROVED** by structural induction on `xs` using `length_nonneg` |
| `pad_to`: `n < 0 ∨ length(self(xs, n)) = n` | UNSUPPORTED | **PROVED** by induction on `n` — the measure rung, now lowering lists |
| `from_digits ≥ 0` (nat result) | UNVERIFIABLE | **SOUND** by induction on the structure of `ds`, the element domain stated |
| `digits_roundtrip`: `from_digits(b, digits(b, n)) = n` | UNSUPPORTED (callee contract "not available") | UNKNOWN — the callees' contracts are in the fragment now and say nothing that decides it (below) |

Totals: refinements **19 SOUND / 2** (was 18 / 3); laws **16 PROVED /
14 UNKNOWN / 1 UNSUPPORTED** (was 13 / 13 / 5); 41/41 run, 41/41 certify,
termination and complexity unchanged at 41/41.

**What the first measurement found.** The three length laws were never structural-backend
material in the first place: the backend's lowering reads one head with all its arguments, the
surface parser emits curried spines (`((cons h) t)`), and the backend's entry points — unlike the
first-order prover's — never normalized them. Every surface-authored cons-recursive body read
"application with no resolvable head", and the CLI then printed the NUMERIC rung's refusal over
it ("no provable numeric measure"), so the module's report named the wrong rung for a whole class
of laws. A second gap of the same kind: predicate-form (bare) literals — `pad_to`'s `lt(n, 0)` —
read "unsupported literal kind: None" in the backend. Both normalized at entry; the CLI now reports
both rungs' reasons when both refuse. With that alone `scale_mod` proved; `pointwise_mod` needed
two more things — the ∀-generalized induction hypothesis (the backend asserted it for folds only;
a `self` that tails BOTH lists needs it, as a fold's accumulator does) and the base-case lemma
`0 ≤ length(ys)`, which no unfolding of a symbolic list yields (`length_nonneg`, now in the
catalog, proved by its own induction before use).

**`pad_to` is the measure rung's.** Its recursion counts `n` down while consing (and resets `xs`
to `nil` in one branch), so structural induction on `xs` is the wrong principle — base case
`length(self(nil, n)) = n` is itself an induction on `n` — and the measure rung had lists out of
its fragment. It lowers them now (the structural backend's `Lst` datatype and definitions, as the
solver's own), while the **first-order prover keeps refusing lists**, so a plain list law still
reaches the structural backend first and every earlier list proof keeps its path. The CLI and
`check-refinement` also run the measure rung after a structural induction that ended UNKNOWN, not
only after one that refused. 21 s end to end (the structural attempt's lemma search is most of it).

**`from_digits ≥ 0` needed the structural measure, stated the right way.** The recursion tails
`ds` (position 1) — no numeric measure, and `head ds` in the step. The termination checker now
reports a structural measure, and the first encoding tried — the quantified hypothesis guarded by
`length(ds') < length(ds)` — is exactly what z3 cannot discharge over the recursive `length`
definition: measured, the obligation closes in 10 ms without that guard and times out at 120 s
with it; the nonlinear `b · self(b, tail ds)` is not the problem. So the hypothesis is stated as
explicit **instances** at each tail-descending recursive call, guarded by the descended list being
a cons (on `nil` the selector is an unconstrained value — asserting the instance there would be
unsound) — the form in which the checker proved the descent, and the strong structural induction
principle instantiated. One more fact was missing: over Int-element lists the law is FALSE
(`ds = [−1]`); the declared `List nat` domain is now stated as `all_nat ds` (every element ≥ 0),
the list analogue of a `nat` parameter's `≥ 0`, carried into the hypothesis instances. The false
strengthening `≥ 1` and the Int-element variant stay UNKNOWN (pinned by tests).

**`digits_roundtrip` is honestly UNKNOWN, not UNSUPPORTED.** With `List nat` in the contract
fragment both callees have contracts, but neither states anything that decides the law
(`digits` has no postcondition; `from_digits ≥ 0` does not relate the two), and the law's own body
is not recursive — no measure to induct on. Proving it needs the callees' DEFINITIONS unfolded
under an induction on `n`, a rung this module does not reach. The previous refusal said "pass
--records to use it" when `--records` WAS passed; a callee without a usable contract now refuses
with the real reason (its signature leaves the fragment, it is absent, a cycle).

### Argued

- **The ordering is the design.** First-order, then structural induction, then the measure rung:
  each later rung lowers strictly more, and the first-order prover's continued refusal of lists is
  what keeps the structural backend's proven path for plain list laws. Admitting lists everywhere
  would have turned `length(append(xs, ys)) = …` into a first-order UNKNOWN that the ladder never
  escalates.
- **Instances over quantifiers, again.** The equivalence prover learned that z3's recfun engine
  will not unfold at `(+ n 1)`; the numeric rung learned that the quantified hypothesis obscures
  `gcd`'s commutativity; this rung learned that a `length`-guarded quantifier over a recursive
  definition does not fire. Each time the fix was the same: say explicitly what the induction
  principle grants at the points the proof uses, and let the solver do arithmetic, not search.
- **The declared domain is part of the obligation.** `nat` parameters already entered as `≥ 0`;
  `List nat` now enters as `all_nat`. A law can be true on the declared domain and false off it,
  and the certificate shows which domain the proof is over.

### Still open after the fifth follow-up (the first closed in the sixth, below)

- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, `digit_sum ≥ 0`, the `mod`
  congruences, Bézout and CRT as checker laws, `pow_mod_agree`/`prime_agree`): both solvers time
  out; a lemma rung (stating `mod` distributivity as axioms) is the next lever if a driver appears.
- `digits_roundtrip`: a non-recursive law whose truth lives in two callees' definitions —
  definitional unfolding of linked callees under an induction on a law variable, a rung not built.
- `isqrt(n·n) = n`: a compound measure (`self` at `n·n`) — needs the two-measure principle.

## Sixth follow-up (2026-10-17, same day): contracts at the point of use

### Measured

| law | before | now |
|---|---|---|
| `isqrt`: `n < 0 ∨ isqrt(n·n) = n` | UNSUPPORTED (compound measure) | **PROVED** from `isqrt`'s own certified contract — the floor bounds, instantiated at `n·n` |
| `bezout_holds`: `a < 0 ∨ b < 0 ∨ self(a, b)` (new, the nat-qualified law) | — | **PROVED**: `ext_gcd`'s Bézout post, instantiated at the body's application |
| `bezout_holds`: `self(a, b)` over all ints | UNKNOWN | UNKNOWN — the post is certified for nats; the prover will not extend it (the −12 example is generative evidence only) |
| `inverse_holds`, `crt_holds`, `pow_mod_agree`, `prime_agree`, `lcm`'s `multiple_of_both`, `digits_roundtrip` | UNKNOWN | UNKNOWN — their callees' contracts state nothing that decides them (below) |

Totals: laws **18 PROVED / 14 UNKNOWN / 0 UNSUPPORTED** (was 16 / 14 / 1,
with one law added); everything else unchanged (41/41 run, certify, terminate, complexity;
refinements 19 / 2).

**Quantifiers were the obstacle, twice more.** The compound-measure law `isqrt(n·n) = n` needs no
induction at all: it follows from `r² ≤ x < (r+1)²` at `x = n·n` (then `r ≤ n < r + 1`). Those
bounds were already PROVED about `isqrt` — a post and a law — and a callee's contract already
entered a dependent's proof as quantified axioms. Stated that way about `isqrt` itself, z3 answers
*unknown*: nonlinear arithmetic under a quantifier. Stated as the one instance the law uses, unsat
in 10 ms. So every contract fact is now also written at each application the obligation makes,
and an obligation z3 leaves unknown with the quantified axioms is retried on the instances alone
(a strictly weaker set, so as sound). The second obstacle was scope: a checker record applies its
callee inside its body — `case ext_gcd(a, b) of (g, (x, y)) ⇒ …` — which the first-order prover
inlines in parameter scope, where no instance could land. The body's applications are now
instantiated at every `self` application the goal makes, `let`-bound over the parameters: the
discipline the induction rung already used for unfoldings.

**The self-contract rung.** `prove`'s ladder ends with the record's OWN certified contract —
posts and laws re-derived by the linked view, exactly as for a callee (without `--records`, a
one-record view of the record and body in hand) — with `self` uninterpreted under it. Nothing
unproved enters; a satisfiable negation is UNKNOWN. The false `isqrt(n·n) = n + 1` stays UNKNOWN.

**What stays UNKNOWN says what the contracts lack, record by record.** `bezout_holds` over all
ints: `ext_gcd` is typed `nat → nat → …`, so its post is certified there and nowhere else; the
checker's own law over ints is true (exhaustive on the bounded domain, the −12 example runs) but
not a consequence of anything certified — the nat-qualified law is the one the commons can
vouch for. `inverse_holds` and `crt_holds` check `mod_inverse` and `crt2`, which carry NO
postcondition (a `Maybe`-valued contract — `Just(x) ⇒ a·x ≡ 1 (mod m)` — is statable now, and
is the next authoring step; whether its nonlinear proof closes is the open question).
`pow_mod_agree` and `prime_agree` compare two definitions; a contract bounds a result, it does
not pin one — that is the equivalence prover's job, and both recursions are numeric (its fragment
is lists). `lcm`'s `multiple_of_both` needs `gcd` divides both arguments, itself UNKNOWN
(nonlinear). `digits_roundtrip` needs the callees' definitions, not their contracts.

### Argued

- **The point of use is where a fact is cheap.** Three rungs now share one idiom — unfoldings
  instantiated at the recursive calls, the structural hypothesis instantiated at the tail
  descents, contracts instantiated at the applications — and each was measured against its
  quantified form first. Quantifiers remain in the certificate (they are the general statement);
  the instances are what the solver uses.
- **A contract's domain is the proof's domain.** The int-typed checker and the nat-typed callee
  are both legitimate records; the law that bridges them has to say on which domain it claims,
  and the prover holds it to the callee's certificate rather than extending the certificate.

### Still open after the sixth follow-up (the last closed in the seventh, below)

- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, `digit_sum ≥ 0`, the `mod`
  congruences, `gcd` divides both): both solvers time out; a lemma rung (stating `mod`
  distributivity as axioms) is the next lever if a driver appears.
- `Maybe`-valued contracts for `mod_inverse` and `crt2` (authoring, then a nonlinear proof).
- `pow_mod_agree` / `prime_agree`: numeric two-recursive equivalence (the equivalence prover
  inducts over lists); `digits_roundtrip`: definitional unfolding of callees under an induction
  on a law variable.

## Seventh follow-up (2026-10-17, same day): induction through a callee's definition

### Measured

| law | before | now |
|---|---|---|
| `digits_roundtrip`: `from_digits(b, digits(b, n)) = n` | UNKNOWN (the callees' contracts say nothing that decides it) | **PROVED** by induction on `n` through `digits`' definition, 5.8 s |
| `bezout_holds`: `self(a, b)` over ALL ints | UNKNOWN (the post is certified for nats) | **PROVED** by unfolding alone: two levels of `ext_gcd`'s definition put the recursive call on `(b, a mod b)` then `(a mod b, b mod (a mod b))` — non-negative by Euclidean `mod` — where the nat-certified Bézout post applies; the `b = 0` and `a mod b = 0` arms are literal |

Totals: laws **20 PROVED / 12 UNKNOWN / 0 UNSUPPORTED** (was 18 / 14 / 0) — the
checker law the previous follow-up left UNKNOWN by design closes too, not by extending the
contract but by reaching the domain where it holds; everything else unchanged (41/41 run, certify, terminate, complexity; refinements 19 / 2).

**The proof the record could not state.** `digits_roundtrip` is not recursive, so none of the
induction rungs applied to it, and composition by contract could not reach it: `digits` carries
no postcondition that relates it to `from_digits`, and no contract could — the fact is the
recursion itself, `digits(b, n) = cons(n mod b, digits(b, n div b))` for `b ≥ 2, n ≥ 1`, and
`from_digits` consuming that cons. So the rung inlines the checker (as the first-order prover
does), unfolds each callee's defining equation at the applications the goal makes and one level
deeper at the recursive calls inside (the numeric rung's discipline), and inducts on the law
variable that reaches a recursive callee's measure position — `n`, through `digits`, whose
measure the termination checker already knows. The hypothesis is an instance at `digits`'
recursive argument `n div b`, guarded by `1 ≤ n div b < n`; with it, `n mod b + b · (n div b) =
n` closes the step and `digits(b, 0) = nil`, `from_digits(b, nil) = 0` the base. Measured, once
more: that instance is unsat in 14 ms; the same hypothesis quantified times out — and so does
the callee's quantified `from_digits ≥ 0` axiom beside it, so the contract ladder (quantified,
then instances only) runs in this rung too. The false round-trip `… = n + 1` stays UNKNOWN.

### Argued

- **A contract is a summary; a definition is the thing.** Composition by contract lets a
  dependent be verified without re-proving its callees, and the previous follow-up showed what
  that costs: a law whose truth is not SUMMARIZED by any certified post cannot be reached. This
  rung reaches it by re-deriving the callees' definitions from their bodies over the linked view —
  the same re-derivation discipline as termination and complexity folding — so the proof depends
  on exactly what the callee IS, at its address, never on a declaration.
- **Four rungs, one idiom.** Unfoldings at the recursive calls, the structural hypothesis at the
  tail descents, contracts at the applications, and now a callee's definition and the hypothesis
  at ITS recursive call: each a quantified statement instantiated at the points the proof uses,
  each measured against the quantified form first.

### Still open

- The nonlinear UNKNOWNs (Pascal, associativity, `binomial ≥ 0`, `digit_sum ≥ 0`, the `mod`
  congruences, `gcd` divides both): both solvers time out; a lemma rung (stating `mod`
  distributivity as axioms) is the next lever if a driver appears.
- `Maybe`-valued contracts for `mod_inverse` and `crt2` (authoring; measured by hand: the
  `Just(x) ⇒ a·x ≡ 1 (mod m)` post from `ext_gcd`'s Bézout identity is satisfiable without
  `m ≥ 2` and times out with it — nonlinear `mod`).
- `pow_mod_agree` / `prime_agree`: numeric two-recursive equivalence (both sides recurse on `e`
  / `n` with different strides; the equivalence prover inducts over lists). The rung above
  reaches ONE recursive callee's measure; a law comparing two needs their definitions unfolded
  in lockstep — the two-recursive principle, numerically.

## Open questions (as they stood before the follow-up)

- **Folding certified callees into termination and complexity.** Eighteen of twenty UNVERIFIABLE
  termination verdicts (and 18 of 21 on complexity) are composed records whose callees are
  themselves certified SOUND. Settled by PREFERENCE, so stated with a recommendation: with
  `--records`, a `fn_ref` callee whose own `check-termination` is SOUND should count as a
  terminating first-order step (and its inferred complexity class as the step's cost), the way
  `--records` already folds declared effects. The argument against: it makes a verdict depend on
  another record's analysis rather than on the body alone, and a callee re-minted with a weaker
  verdict would silently weaken its dependents' verdicts — so the fold should re-run the callee's
  check, not read its declaration. → **Built (re-derivation), above.**
- **Numeric induction in `prove`.** The laws worth proving here (`gcd` commutativity, Pascal,
  Bézout, `isqrt` bounds, `mod_pow ≡ pow_mod_naive`) are all about recursive `nat` functions; the
  inductive backend has the list principle and the equivalence prover a constant-descent numeric
  one. → **Built (strong induction over the checker's measure), above; the two-measure laws remain.**
- **Variable divisors.** `digits`/`digit_sum` recurse on `div(n, b)` with the base `b` a parameter;
  the guard `b < 2 ⇒ nil` is in the body, so the descent is well-founded, but the checker reads
  only literal divisors. → **Built (a divisor's own floor ≥ 2), above: both SOUND on termination and complexity.**
- **The exhaustive domain.** −4..4 / 0..8 is right for list and string laws; for number theory a
  record-declared sample range (or sampling across magnitudes) would make the generative rung see
  past the trivial cases without pretending to be a proof. → **Built (`--wide`), above.**

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/integer-tier-poc/build_records.py            # z3 on PATH for the prove rung
python3 evolution/integer-tier-poc/build_records.py --no-prove # without a solver
```

The builder re-mints every record (addresses are deterministic), runs validate / run / certify /
check-properties (generative, 400 cases, linked over `records/`) / prove, and rewrites
`records/REPORT.json` and `records/REPORT.md`. The records are published on Arca
(`https://nl.1105software.com`) with bodies and signed certifications; discover them by tag
(`math/number-theory/<name>`, e.g. `math/number-theory/is-prime`). No credential is needed for
anything here.
