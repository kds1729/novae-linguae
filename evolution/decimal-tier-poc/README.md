# A decimal/money tier: rounding modes, rescaling, rates, exact allocation and amount text — what an agent handling money needs, certified

- **Status:** published (2026-11-02). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-11-02, last updated 2026-11-02 (round 5 — tie posts, named parameters)
- **Scope:** `tooling/validator` (the complexity fold, the prover's callee preamble, the refinement
  checker's rungs), `spec/evaluation.md`.
- **Provenance:** upstream commit `40765fb` plus the four validator changes this module forced
  (listed under *What was measured*; committed with the module); nl-validator 0.1.7 in-tree,
  derived-cache version 9; z3 4.13.3 (Debian/Ubuntu package); python 3.14 for the builder, whose
  reference values come from Python's `decimal` module (`ROUND_FLOOR`, `ROUND_CEILING`,
  `ROUND_DOWN`, `ROUND_HALF_UP`, `ROUND_HALF_EVEN`) and exact integer arithmetic; one laptop
  (Ubuntu 26.04).
- **Resolution:** —

## Summary

The number tiers before this one (`integer-tier-poc`, `rational-tier-poc`, `gaussian-tier-poc`)
proved hard mathematics; this module asks the practical question those capabilities were for:
**can an agent that handles money get certified arithmetic for it?** Money is represented as
INTEGER MINOR UNITS at a decimal scale (12.34 at scale 2 is `1234`) — the representation payment
systems use — so every operation is exact integer arithmetic plus an explicit rounding.

Sixteen pure records, authored in surface syntax, no new builtin: the five rounding modes as
divisions (`div_floor`, `div_ceil`, `div_trunc`, `div_half_up`, `div_half_even`), `pow10`,
`rescale` between scales, `apply_rate` / `percent_bps` / `fx_convert` (tax, discount, interest,
currency conversion with an exact rational rate), `list_sum`, `alloc_from` + `allocate` (split a
total over weights), and `format_amount` / `parse_amount` / `amount_text_roundtrip` (the text
boundary, where an agent meets amounts in JSON, CSV and API payloads).

**Round 6: 19/19 run and certify; 23 of 23 contracts SOUND (round 5 added the tie rule to every rounding record, round 6 decimal-text entry points and parameter docs); termination 16/16; complexity 15/16;
laws 22 PROVED / 1 UNKNOWN / 0 UNSUPPORTED of 23, all 23 held generatively (the text roundtrip's
wide sample mostly skipped — see *Still open*).** The guarantees an
agent most needs are certified, not tested:

- **Every rounding mode's definition is its contract** — e.g. half-even: `2·|n − q·d| ≤ d`, and
  on a tie `q` is even — and exactness (`round(k·d / d) = k`), monotonicity and sign symmetry are
  PROVED laws.
- **Every rate application is within half a minor unit** of the exact value:
  `2·|a·num − result·den| ≤ den` on `apply_rate`, the same through `pow10` on `fx_convert`.
- **No information is lost going to more decimal places**: `rescale(rescale(u, f, t), t, f) = u`
  for `t ≥ f`, PROVED from `div_half_even`'s exactness law and `pow10`'s laws.
- **No lost cents**: `allocate(t, ws)` returns one part per weight whose parts SUM EXACTLY to `t`
  — both posts SOUND, by the cumulative-rounding method (`part_i = ⌊t·C_i/W⌋ − ⌊t·C_{i−1}/W⌋`)
  whose telescoping invariant is a PROVED law on `alloc_from`.

The one UNKNOWN is the text roundtrip (`parse(format(u, k), k) = u`): it holds on every generated
case, but `to_string`/`str_split` have no SMT theory counterpart — a boundary, stated below.

Getting there cost **four validator changes, one of them a real defect** (a callee whose contract
is laws alone — no post — was invisible to every caller's proof), and publishing found a fifth:
**schema validation was exponential in nesting depth**, so the commons gate refused the deepest
body.

## What was measured

All numbers from `python3 /home/claude/sandbox/run_module_builder_guarded.py decimal-tier-poc
<log>` (the builder inside a 9 GiB memory-capped cgroup), validator built from the working tree.

| round | change | result |
|---|---|---|
| 1 | first authoring (13 records, no text) | 13/13 certified; every rounding contract SOUND and every rounding law PROVED on first contact; `pow10`'s `product` law (`10^(a+b) = 10^a·10^b`) PROVED by numeric induction; `rescale` + `fx_convert` complexity UNVERIFIABLE; `rescale`'s roundtrip and `alloc_from`'s telescoping UNKNOWN; `allocate`'s two posts UNVERIFIABLE |
| 2 | validator change 1; unfolding laws on `list_sum`; text records | `rescale`/`fx_convert` complexity SOUND; `list_sum`'s `cons` law PROVED; telescoping still UNKNOWN; closed laws (`self(0) == 1`) refused "not a `forall`" by the induction backends; `zeros` (a padding loop) honestly O(n²); `parse_amount` 4/9 examples failed (argument order) |
| 3 | validator changes 2–4; laws restated in guarded `forall` form; derived-cache version 9 | `rescale` roundtrip PROVED; telescoping PROVED; `allocate` both posts SOUND, complexity SOUND; `parse_amount` 1/9 failed — accepted `"1-2"` as −2.00 (a real bug in the record: the sign's position was never checked) |
| 4 | sign accepted only as the first character | 16/16 run + certified, 17/17 contracts SOUND, laws 22/1/0; 4.3 min, peak 1.6 GB (one z3 query) |
| 5 | the TIE RULE as a post on `rescale`, `apply_rate`, `percent_bps`, `fx_convert` (`2·|err| ≠ unit ∨ result even`); parameters named for what they are (`amount from_places to_places`, `amount rate_num rate_den`, `total weights`, …); `fx_convert` gains the €250 @ 1.0825 tie example | 16/16 certified, **21/21 contracts SOUND** — each tie post derived from `div_half_even`'s own; laws 22/1/0; 5.8 min. Why: a stranger agent on unseen goals (banker's rounding, FX) could not tell half-even from half-up from these contracts (they bounded only the error) and read the bodies; and could not tell `fs`/`ts` were decimal-place counts |

**The validator changes** (each with a regression test; suite 505 → 509 lib tests, all green):

1. **A constant-time callee folds over any argument** (`complexity.rs`). `rescale` applies
   `div_half_even(u, pow10(k))`; the fold refused because `pow10(k)` is exponential in `k`. An
   `O(1)` callee is `O(1)` whatever it is given. Test:
   `a_constant_callee_folds_over_any_argument_a_log_one_does_not`.
2. **Only the positions a callee's cost depends on need be bounded** (`complexity::cost_positions`,
   `LinkedCallees::cost_positions`). A callee whose every step is a constant-time builtin or a
   self-call costs its recursion depth, and the depth is bounded by its descent position alone.
   `allocate` passes `alloc_from` the weight total `list_sum(ws)` at a position the recursion never
   descends on. Test: `a_callees_cost_depends_only_on_its_descent_position_when_its_steps_are_constant`.
   Also: `str_split` joins the size-bounded ops (every piece is no longer than the string).
3. **DEFECT: a contract of laws alone was dropped** (`prove.rs`, the callee preamble). The
   preamble skipped every callee contract without a post — its LAWS included. `list_sum` has no
   post, only its proved unfolding law `self(cons(x, xs)) = x + self(xs)`, so `alloc_from`'s
   telescoping proof saw `list_sum` as a bare uninterpreted function. Hand-measured: the same
   induction step with the law as an axiom is unsat in 0.64 s. Test:
   `a_callee_contract_of_laws_alone_is_axiomatized`. Since this changes what a derivation proves,
   `DERIVATION_VERSION` 8 → 9.
4. **A first-order rung with lists admitted, last in the refinement checker**
   (`prove::prove_property_with_lists`, `refine::decide`). `allocate`'s `length(result) ==
   length(ws)` is one instance of `alloc_from`'s PROVED length law — no induction of its own — yet
   fell through every rung: first order refuses lists (deliberately: that refusal routes a list law
   to the structural backend), the structural backend refuses a callee, the numeric one a
   non-recursive body. The new rung runs only after all of them refused, only for a non-recursive
   body. Test: `lists_are_admitted_in_first_order_only_on_request_and_never_for_a_recursive_body`.

**Cross-module check:** the Gaussian (1 min) and integer (45 min) tiers were rebuilt on the
changed validator — both byte-identical to their committed records and reports.

**Publishing found a fifth defect, in the spec's schemas.** Arca's gate refused
`body-parse_amount.json` 422 `schema_invalid`, and locally `nl-validator validate` was killed at
3 GB on it. Every expression schema dispatched on `kind` through a `oneOf`; a failing branch still
descended into properties it shares with the matching one (`let` and `lambda` both have `body`),
and every branch's errors were collected — exponential in nesting depth. Measured with alternating
`let`/`lambda` nests under a 3 GiB cap: the old body schema aborted at let-depth 8; the rewrite
(`if`/`then` on `kind`, equivalent because the `kind` consts are disjoint; 12 blocks across seven
schemas) validates let-depth 24 in 0.07 s under 100 MB. `format_amount`'s body (JSON depth 23)
had passed — at 2.4 GB. Both nodes were redeployed for gate parity and all 16 records, bodies and
certifications are on Arca (publish logs `decimal_publish_round4{,b}.log`).

## What is argued

- **The earlier tiers' prover work transferred.** The rounding-mode contracts — nonlinear
  inequalities over `div`/`mod` — closed on first contact; `rescale`'s roundtrip and the
  allocation's telescoping both needed the Euclidean rung's stepwise checker, built for the
  integer tier. None of the four changes here was a new proof technique; they were plumbing
  (two complexity-fold widenings, one dropped contract, one missing rung for an easy obligation).
  *Inference:* on practical money code, the prover's strength was already sufficient; its gaps
  were in how facts flow between records.
- **Defect 3 was invisible until a law-only helper existed.** Every callee in the three number
  tiers carried at least one post, so the skip never fired. A small helper whose only facts are
  laws — `list_sum`, the most ordinary function there is — is exactly what practical code is
  made of.
- **The record-level bug found in round 3 is the argument for examples AND laws.** `parse_amount`
  passed eight examples and accepted `"1-2"`; one adversarial example caught it. A parse/format
  law would have caught it too, had strings been provable (they are not — below).

## Still open

- **The text roundtrip is not provable**: `to_string`, `str_split`, `str_join` and `parse_int`
  have no sound SMT counterpart (the fragment admits `str_concat`/`str_length`/`str_contains`/
  `str_lt` only). It is held generatively: 81 exhaustive small cases; the wide sampler held 34 and
  SKIPPED 166 (a random scale in the trillions makes `pow10`'s linear recursion exceed the
  evaluator's 100,000-application depth — realistic scales are under 20). A
  string-to-digits theory — or proving `format`/`parse` against a digit-list model — is the
  named rung.
- **`amount_text_roundtrip`'s complexity is UNVERIFIABLE**: the fold cannot bound the length of
  `format_amount`'s output (it contains `to_string` of `10^k + r`). Honest, stated, not forced.
- **Closed laws on recursive bodies** (`self(0) == 1`) are refused by the induction backends as
  "not a `forall`"; stated here in guarded `forall` form instead. The first-order prover accepts
  closed laws since the Gaussian tier; the induction backends do not yet.
- **Nonnegative parts**: `allocate` gives every part ≥ 0 for `t ≥ 0` and nonnegative weights, but
  stating it needs a quantifier over list elements (`all_nat` exists inside the prover, not in the
  predicate language).
- **Bounded integers**: money here is unbounded `int`; an agent emitting minor units into an
  `int64` field needs an overflow bound — the fixed-width tier is the natural next module.

## Reproducing

```
cargo build --release --manifest-path tooling/validator/Cargo.toml
python3 evolution/decimal-tier-poc/build_records.py            # mint + gauntlet (needs z3; ~4 min, < 2 GB)
python3 tooling/commons-node/publish_records.py evolution/decimal-tier-poc/records --sign <seed>
```

No credential or network is needed for the builder; the publish step needs a node.
