# A certified integer tier: number theory and the NTT as in-language records, and what the gauntlet found

- **Status:** absorbed (2026-10-17). Every language-level finding below is in the tree: eight
  evaluator/checker defects fixed and two checkers widened (`tooling/validator`), the records
  published to the commons with signed certifications. The 38 records and their builder live here
  (`records/`, `build_records.py`) — reproducible with no credential.
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

## Open questions

- **Folding certified callees into termination and complexity.** Eighteen of twenty UNVERIFIABLE
  termination verdicts (and 18 of 21 on complexity) are composed records whose callees are
  themselves certified SOUND. Settled by PREFERENCE, so stated with a recommendation: with
  `--records`, a `fn_ref` callee whose own `check-termination` is SOUND should count as a
  terminating first-order step (and its inferred complexity class as the step's cost), the way
  `--records` already folds declared effects. The argument against: it makes a verdict depend on
  another record's analysis rather than on the body alone, and a callee re-minted with a weaker
  verdict would silently weaken its dependents' verdicts — so the fold should re-run the callee's
  check, not read its declaration. Not built here; the module measures the gap.
- **Numeric induction in `prove`.** The laws worth proving here (`gcd` commutativity, Pascal,
  Bézout, `isqrt` bounds, `mod_pow ≡ pow_mod_naive`) are all about recursive `nat` functions; the
  inductive backend has the list principle and the equivalence prover a constant-descent numeric
  one. A `nat` induction principle with a halving/mod step is the named next rung.
- **Variable divisors.** `digits`/`digit_sum` recurse on `div(n, b)` with the base `b` a parameter;
  the guard `b < 2 ⇒ nil` is in the body, so the descent is well-founded, but the checker reads
  only literal divisors. A floor on the divisor (`b ≥ 2` from the guard) would close it.
- **The exhaustive domain.** −4..4 / 0..8 is right for list and string laws; for number theory a
  record-declared sample range (or sampling across magnitudes) would make the generative rung see
  past the trivial cases without pretending to be a proof.

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
