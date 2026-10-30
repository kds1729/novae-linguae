# An exact rational tier over the integer tier's certified `gcd`: cross-module composition by content address, and what the contracts let the prover conclude

- **Status:** published (2026-10-30). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-30, last updated 2026-10-30
- **Scope:** `tooling/validator` (the prover's contract composition, the divisibility rung, the
  generative checker), `evolution/integer-tier-poc` (its `gcd` record, applied by address),
  `spec/evaluation.md`.
- **Provenance:** upstream commit `a69b64e` (nl-validator 0.1.4); the integer tier's `gcd` at
  `fn_7875d6be2650b6699…` (its eleventh-follow-up record, with the greatest-divisor law); z3
  4.13.3 (Debian/Ubuntu package); python 3.14 for the builder, whose reference values are
  Python's exact `fractions.Fraction`; one laptop (Ubuntu 26.04).
- **Resolution:** —

## Summary

Fourteen pure records — `rat_normalize` (sign to the numerator, both components divided by the
integer tier's `gcd`), negation, reciprocal, add/sub/mul, equality and order by
cross-multiplication, floor, and five checker records for the algebra — were authored in surface
syntax over `(int, int)` pairs and put through the gauntlet. **14/14 run and certify; every
declared contract is SOUND (10 posts) — including the value-preserving identities
`fst(result)·(b·d) = (a·d + c·b)·snd(result)` on the arithmetic records, each derived from
`rat_normalize`'s own proved law at the body's application, through `gcd`'s certified contract
fetched from another module by content address: cross-module composition of contracts, the thing
this module exists to test, works.** Laws: **14 PROVED / 6 UNKNOWN / 0 UNSUPPORTED** of 20, all 20
held generatively with nothing skipped. The proved set includes the cross-multiplication identity
`p·d = n·q` for normalization (first-order, through the Euclidean rung), the Euclidean floor
bounds, and every first-order law about the order and the equality. The six UNKNOWNs are one
fact and its consequences: **the normal form is in lowest terms** (`gcd(|p|, q) = 1`) — true by
`gcd`'s greatest-divisor law, but its proof needs the product `e·g` as a divisor witness, which
the divisibility rung does not form — and, resting on it, uniqueness of lowest terms, which is
what `x + 0 = x`, `x·1 = x`, `(x + y) + z = x + (y + z)` and "equal values have equal normal forms"
all reduce to once the value posts are in place. The gauntlet also found **one validator defect**,
fixed in-tree (8785d85): a callee's contract instance at a pattern-bound argument was stated
outside the pattern's scope (`unknown constant p`), so the lowest-terms law read UNSUPPORTED
before it could read UNKNOWN. And one cost is measured: the complexity fold is UNVERIFIABLE for
every record that passes a product to a callee (7 of 14) — the fold bounds a callee's class only
for size-bounded arguments, and `a·d + c·b` is not one.

## Provenance of the inputs

| input | pinned at |
|---|---|
| `nl-validator` | 0.1.4, commit `a69b64e`; the laws proved with z3 4.13.3 |
| the integer tier's `gcd` | `fn_7875d6be2650b6699…` — copied record and body into `records/` so `--records` resolves the cross-module `fn_ref`; the address is the same, so publishing it again is idempotent |
| reference values | `fractions.Fraction` (CPython 3.14): every worked example is recomputable |
| the builder | `build_records.py` here — re-mints deterministically; rerun after any validator change |

## What was measured

Builder round 2 (`records/REPORT.{json,md}`; the log in the author's sandbox), 30.6 min, z3 4.13.3:

| record | signature | contracts | laws (prove) | generative |
|---|---|---|---|---|
| `rat_normalize` | `int int → (int, int)`, pre `d ≠ 0` | `snd ≥ 1` SOUND; `n = 0 ⇒ result = (0, 1)` SOUND | `value_preserved` **PROVED** [Euclid]; `idempotent` UNKNOWN | EXHAUSTIVE 81 / 81 |
| `rat_coprime_holds` | checker | — | `lowest_terms` UNKNOWN (round 1: UNSUPPORTED, the scoping defect) | EXHAUSTIVE 81 |
| `rat_neg` | | `snd = d` SOUND | involution ×2 **PROVED** | EXHAUSTIVE 81 ×2 |
| `rat_inv` | pre `n ≠ 0` | `fst·n = d·snd` SOUND | `denominator_positive` **PROVED** (from normalize's post) | EXHAUSTIVE 81 |
| `rat_add` | 4 ints, pre `b, d ≥ 1` | `snd ≥ 1`, `fst·(b·d) = (a·d + c·b)·snd` both SOUND | `commutative` **PROVED** | HELD 400 |
| `rat_sub` | | both SOUND | `self_difference_is_zero` **PROVED** (through normalize's zero post) | EXHAUSTIVE 81 |
| `rat_mul` | | both SOUND | `commutative`, `zero_annihilates` **PROVED** | HELD 400, EXHAUSTIVE 729 |
| `rat_eq` | 4 ints → bool | — | `reflexive`, `symmetric` **PROVED** | EXHAUSTIVE 81, HELD 400 |
| `rat_lt` | pre `b, d ≥ 1` | — | `irreflexive`, `asymmetric` **PROVED** | EXHAUSTIVE 81, HELD 400 |
| `rat_floor` | pre `d ≥ 1` | — | `lower_bound`, `upper_bound` **PROVED** | EXHAUSTIVE 81 ×2 |
| `rat_add_zero_holds`, `rat_mul_one_holds`, `rat_eq_normalize_holds`, `rat_add_assoc_holds` | checkers | — | UNKNOWN ×4 | EXHAUSTIVE 81, 81; HELD 400, 400 |

Totals: 14/14 run PASS, 14/14 certified, termination SOUND 14/14, complexity SOUND 7 /
UNVERIFIABLE 7 (every record passing a product to a callee: add/sub/mul and the checkers over
them, plus `rat_coprime_holds`), contracts 10 SOUND / 0, laws 14 PROVED / 6 UNKNOWN / 0
UNSUPPORTED, 20/20 held (bounded) and 20/20 held (`--wide`, sampled across magnitudes). No skipped
cases anywhere: every law is guarded (`d == 0 ||`, `b < 1 ||`), and the guards decide.

Round 1 differed in two lines: `lowest_terms` read `UNSUPPORTED solver error: … (error "line 31
column 41: unknown constant p")`, and the three arithmetic records had only their `snd ≥ 1` posts.

**The defect, reproduced small.** `let p = f(n) in f(p) + 1 > 0` with `f`'s contract `result ≥ 0`:
the contract instance at `f(p)` was emitted at top level, outside the `let`. The fix states it at
the binding's definition, `f(f(n))` (test `a_callee_instance_at_a_local_binding_is_stated_at_its_definition`).

**What the lowest-terms proof needs, by hand.** Let `g = gcd(|n|, |d|)`, `p = n div g`, `q = d
div g`, `e = gcd(|p|, q)`. Then `e | p` and `e | q`, so `e·g | n` and `e·g | d` (a product of two
witnesses), so `e·g | g` by the greatest-divisor law at `d = e·g`, so `e·g ≤ g`, so `e = 1`. The
divisibility rung instantiates a law's free variable over the goal's gcd TERMS; `e·g` is not one.
The step is one lemma away, and the lemma is about products.

## What is argued

- **Contracts compose across modules exactly as within one.** `gcd` was never re-authored here;
  its record and body were copied under their own address, and every fact the rational records
  needed from it arrived through the linked view's re-derivation (and the derived-contract
  cache) — the same path `lcm` uses inside the integer tier. The module boundary is a directory,
  not a semantic line. The one cost is operational: a consumer's `--records` directory must hold
  the callee, which the builder arranges by copying.
- **A contract pins values, not functions — and the value posts are the right contract.** The
  checkers that compose two records (`x + 0 = x`) stayed UNKNOWN even once each side's value was
  certified, because two pairs with the same value are equal only if both are in lowest terms
  and lowest terms are unique. That is the honest shape of the gap: not "the prover is weak on
  composition" but "one number-theoretic fact is missing, and five laws wait on it".
- **The next rung is a product witness.** Both the lowest-terms proof and Euclid's lemma (`q |
  p·s ∧ gcd(p, q) = 1 ⇒ q | s`, what uniqueness needs) hinge on forming `e·g` or `q·k` as a
  divisor and instantiating a divisibility law there. The free-variable mechanism exists; its
  candidate set does not include products of the goal's terms. Bounded (pairs of the goal's gcd
  terms), that is a small extension — and the measurement here says what it would buy: six laws.
- **Complexity through products is a fold limit, not a defect.** The class fold reads a callee's
  cost only for size-bounded arguments; `normalize(a·d + c·b, b·d)` is `O(log(a·d + c·b))`, which
  is `O(log n)` in the inputs' magnitude but not in the fold's vocabulary. Stated, not changed.

## What worked well

- The integer tier's work carried over whole: `gcd`'s posts and the greatest-divisor law were
  simply there, by address. The Euclidean rung proved the cross-multiplication identity on the
  first try, first-order, with no authored lemma.
- The skipped-case report (c1ea3aa) made the guards checkable: every law here is guarded, and
  the report shows zero skips, so the EXHAUSTIVE/HELD verdicts mean what they say.
- The gauntlet found the scoping defect on the first run, with the solver's own error text
  pointing at the pattern variable; the fix was a stack of local bindings in the lowering, and
  the derived-contract cache made the second 30-minute round cheaper than the first.

## Still open

- **`lowest_terms`** (and with it `idempotent`, `add_zero`, `mul_one`, `equality_is_normal_form_
  equality`, `associative`): a product witness `e·g` in the divisibility rung's candidate set, then
  Euclid's lemma for uniqueness. Measured need: six laws.
- **Complexity through product arguments**: seven UNVERIFIABLE folds; a magnitude-based size
  measure for `log` classes would decide them. Driver-gated.
- A Gaussian-integer or Gaussian-rational tier over these pairs is the natural next module: the
  same pair machinery, with `norm = a² + b²` where the integer tier's facts enter.

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/rational-tier-poc/build_records.py            # mint + gauntlet (needs z3)
python3 tooling/commons-node/publish_records.py evolution/rational-tier-poc/records --sign <seed>
```

No credential or network is needed for the builder; the publish step needs a node.
