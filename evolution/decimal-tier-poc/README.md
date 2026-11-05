# A decimal/money tier: rounding modes, rescaling, rates, exact allocation and amount text — what an agent handling money needs, certified

- **Status:** published (2026-11-02). <!-- exploring | published | absorbed | superseded -->
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-11-02, last updated 2026-11-03 (round 8 — the allocation quota; a false name hint corrected)
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

**Round 9: 20/20 run and certify; 30 of 30 contracts SOUND (round 5 added the tie rule to every rounding record, round 6 decimal-text entry points and parameter docs, round 7 the rule on the text records, round 9 a percentage as text); termination 20/20; complexity 16/20;
laws 23 PROVED / 1 UNKNOWN / 0 UNSUPPORTED of 24, all 24 held generatively (the text roundtrip's
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
- **No part is off by a whole unit**: every part is within one unit of its exact share —
  `|part·W − t·w_i| < W`, the `quota` law on `alloc_from`, PROVED. Where the leftover units land
  is the cumulative method's, NOT largest-remainder's (Hamilton's): 10 over six equal weights is
  `[1,2,2,1,2,2]` here, `[2,2,2,2,1,1]` there — a worked example on `allocate` shows it.

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
| 6 | `param_docs` (new optional field, function-record v0.2) on every record — one plain-words line per parameter with its unit; three records that take decimal TEXT: `parse_decimal` ("1.0825" → (10825, 4)), `round_text_half_even`, `fx_convert_text`, each with a post stating it IS the numeric record over the parsed values (so the tie rule is one `refs` hop away) | 19/19 certified, `param_docs` SOUND 19/19, **23/23 contracts SOUND**, laws 22/1/0; complexity 16/19 (the two wrappers and the text roundtrip: a pattern-bound callee result is not size-bounded for the fold); 5.7 min. Needed two validator fixes: an ambiguous bare `None` (two `Maybe` sorts in scope) now takes the declared result sort, and a `replace_token` out-of-bounds panic. Why: stranger 11 inferred `from_places` from the contract and scaled 7.125 / 1.0825 by hand. Not a CLI float literal on purpose — 7.125 as an IEEE float is inexact, and the scale is the function's business |
| 7 | The rounding rule stated ON the text records: `round_text_half_even` and `fx_convert_text` each gain two posts over the parsed digits — within half a unit, and a tie goes to the even result — instead of delegating it to `rescale` / `fx_convert` through `refs` (stranger 12 fetched `rescale` once to learn it). `fx_convert_text`'s are stated for place counts ≥ 0 (always so; but `parse_decimal`'s body takes `head` of a list, outside the refinement fragment, so it cannot promise it, and the guard is what discharges `fx_convert`'s pre `rate_den >= 1`) | 19/19 certified, **27/27 contracts SOUND**, laws 22/1/0, complexity 16/19; 6.6 min; only the two text records re-minted |
| 8 | `allocate`'s name hint `largest_remainder` was FALSE (rounds 1–7) — it is cumulative rounding, a different placement of the leftover units — now `cumulative_rounding`, with a worked example (10 over six → `[1,2,2,1,2,2]`) and the placement in its `param_docs`; `alloc_from` gains the `quota` law (each part within one unit of its share). The quota law first read UNSUPPORTED: the structural backend took `self`'s FIRST argument as the list, and `alloc_from t ws c w` recurses on its second (`t` came out list and int at once) — validator change 5 | 19/19 certified, 27/27 contracts SOUND, **laws 23/1/0** (`quota` PROVED by structural induction; `length` now proves structurally too); 4.7 min; only `alloc_from` + `allocate` re-minted. Why: the remainder-cent question was stranger 13's; reading the body to answer it found the false hint |
| 9 | `percent_of_text` (amount text, percentage text, places): the percentage over 100·10^places through `fx_convert`, with the same three posts as `fx_convert_text` (it IS `fx_convert` over the parsed digits; within half a unit; a tie to the even). Examples include 8.875% of $59.99 → 532 and 1% of $0.50 → 0 (the tie) | 20/20 certified, **30/30 contracts SOUND**, laws unchanged, complexity UNVERIFIABLE like its two text siblings; 7.3 min; nothing else re-minted. Why: stranger 27's 8.875% sales tax is 887.5 basis points — no whole `percent_bps` — so it scaled the amount and rate to `apply_rate` by hand |

**The validator changes** (each with a regression test; suite 505 → 509 lib tests in rounds 1–4, 516 after changes 5–6, all green):

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
5. **Round 8: structural induction over a body that descends on a LATER parameter**
   (`induct::trailing_list_descent`). The backend took `self`'s first argument as the list, so a
   body like `alloc_from t ws c w` (descends on `tail(ws)`, `t` an int) was refused as "variable `t`
   used at conflicting sorts" — every law on it went to the numeric backend or nowhere. The position
   every recursive call tails at is now read off the body and used for the law's sort inference and
   the `define-fun-rec`'s parameter sorts; the induction is still on the law's first list variable,
   with the structural hypothesis. Hand-measured first: the quota's arithmetic core after one
   unfolding is `unsat` in z3 at once — only the sort assignment stood in the way. Test:
   `a_body_descending_on_a_later_parameter_proves_by_structural_induction` (both call forms, and a
   false law stays unproved). `DERIVATION_VERSION` 10 → 11.
6. **DEFECT, found by change 5's cross-check: a non-closing structural induction read VIOLATED**
   (`refine::decide`). The induction backend's `Failed` covers a satisfiable STEP ("does not
   close"), not only a satisfiable base, and its lists are `Lst` of Int — a `List nat`'s elements
   are unconstrained there. The refinement checker reported every `Failed` as a counterexample, a
   hard certification failure. Latent until change 5 let the backend see `from_digits` (nat, List
   nat → nat; descends on its second parameter): the integer tier's rebuild read its `result ≥ 0`
   VIOLATED, 42/43 certified. `Failed` now falls through to the numeric rung like UNKNOWN
   (`equiv.rs` already treated it so); VIOLATED comes only from the first-order rungs. Test:
   `a_structural_induction_that_does_not_close_is_never_a_violation`.

**Cross-module check:** the Gaussian (1 min) and integer (45 min) tiers were rebuilt on the
changed validator — both byte-identical to their committed records and reports. Round 8 (changes
5–6): Gaussian (1.1 min) and rational (44 min) byte-identical; integer first 42/43 (change 6's
defect), then re-run on the fix.

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
