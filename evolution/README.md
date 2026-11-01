# `evolution/` — evidentiary modules

This directory holds **self-contained records of work done against Novae Linguae from the outside**:
what was tried, what was measured, what it implies, and what remains open. Each subdirectory is one
**module**, owned by its author, isolated from every other module and from the rest of the tree.

It exists because the project had no landing zone for this kind of contribution. A body of
evidence is not a patch and not a specification: patching core code needs design agreement before
the evidence has even been read, and writing into `spec/` edits the normative voice of the language.
A module needs neither. It can be reviewed on whether its measurements are sound, merged without
committing the project to anything, and built on by the next person.

## The boundary with `spec/`

| | |
|---|---|
| **`spec/`** | **normative** — what the language and its artifacts *are*. Changing it changes the definition. |
| **`evolution/`** | **evidentiary** — what was tried and observed, by whom, under what conditions, and what the author concludes. Changing it changes nobody's obligations. |

A module **graduates** when its conclusions land somewhere normative — a `spec/` change, a code
change, a pulled primitive. At that point its status becomes `absorbed` and it stays in place as
provenance for the change it caused.

## What belongs here

- Results from running the toolchain at a scale or against inputs the in-repo references don't cover.
- Proposals that need a maintainer decision before any code is worth writing.
- Explorations that reached a negative result. **These are as valuable as the positive ones** and
  have nowhere else to live.

## What does not belong here

- **Fixes for defects.** A bug with a reproduction is an ordinary code PR. A module may *report* a
  defect, but the fix travels separately.
- **Generated corpora.** Records belong published to a commons node — content-addressed, gate-verified,
  replicable — not committed to this repository. A module should say how to regenerate them.
- **Vendor- or product-specific machinery** the project would then have to maintain. Describe the
  pipeline and pin its inputs; keep the pipeline itself in your own repository.

## Required front matter

Every module's `README.md` opens with:

```markdown
- **Status:** exploring | published | absorbed | superseded
- **Author:** name <contact>
- **Dates:** first published YYYY-MM-DD, last updated YYYY-MM-DD
- **Scope:** which parts of the project this touches
- **Provenance:** upstream commit, plus pinned versions/revisions of every external input
- **Resolution:** (once not `exploring`) the PR, commit, or issue that resolved it
```

`Provenance` is not optional and not approximate. A result nobody can reproduce is an anecdote, and
the whole value of a module is that a stranger can re-run it and disagree with you on the evidence.

**`Status` describes the module, not its proposals**, and the two use *different vocabularies* —
because merging a module means its findings are in the tree, and nothing more. Nobody has agreed to
anything by merging it, so the module axis has no word for agreement:

| axis | statuses | what the terminal state means |
|---|---|---|
| **module** | `exploring` → `published` → `absorbed`, plus `superseded` | every proposal it raised has landed or been declined |
| **proposal** (one per file under `proposals/`) | `proposed` → `accepted` \| `declined`, then `absorbed` | the accepted change is actually in the tree |

`published` is deliberately flat: it states the fact that merging establishes — the evidence is here,
reproducible, and open to disagreement — and claims no endorsement of what the author concluded from
it. A module reaches `absorbed` only once its conclusions have landed somewhere normative: a `spec/`
change, a code change, a pulled primitive. A module whose only proposal is `declined` never reaches
`absorbed` and correctly rests at `published` — the evidence stands even where the recommendation
did not.

## The one discipline that matters

**Separate what you measured from what you argue.** State numbers with the command that produced
them; label inferences as inferences; keep recommendations in their own section where a reader can
reject them without discarding the data.

The same discipline applies to a proposal's open questions: **separate what you settled from what
you are asking.** A question a measurement or a precedent settles is the author's to resolve, and
recording it as settled is more useful than asking it. Only a judgement between two goods genuinely
blocks — see [`TEMPLATE.md`](TEMPLATE.md) under *Proposals*. Most proposals collapse to one real
question, and finding that one is the author's job, not the reviewer's.

This is what keeps the directory trustworthy rather than a pile of opinions, and it is what makes a
module worth building on: the measurements stay useful even when the author's conclusions turn out
to be wrong.

## Lifecycle

Statuses move forward; **a module is never deleted.**

```
module:    exploring ──▶ published ──▶ absorbed
                              │
                              ╰──▶ (stays here if every proposal was declined)

proposal:  proposed ──▶ accepted ──▶ absorbed
               │
               ╰──────▶ declined

either ──▶ superseded   (a later module supersedes it; link both ways)
```

The two run independently, so a module sits at `published` with one proposal `declined` and another
still `proposed`, and only moves to `absorbed` once none are outstanding. A declined proposal keeps
its reasoning and gains the reason it was declined. A superseded module links to its successor and
the successor links back. Nothing is rewritten to look correct in hindsight — the same
immutability-plus-lineage discipline that `supersedes` / `derived_from` give a function record,
applied to the work *about* the project. Update a status by editing it in place and filling in
`Resolution`; the history carries what it used to say.

## Adding a module

1. Copy [`TEMPLATE.md`](TEMPLATE.md) to `evolution/<kebab-case-name>/README.md`.
2. Fill in the front matter. Pin your inputs.
3. Put detail in sibling files (`findings.md`, `proposals/NN-name.md`, …) and index them from the
   `README.md`, so the entry point stays readable.
4. Open one PR for the module. If it also reports a defect, open the fix separately and cross-link.

## Modules

| module | status | what |
|---|---|---|
| [`gcp-sdk-poc`](gcp-sdk-poc/) | `published` | A whole cloud API (Google Cloud Storage v1, 81 operations) through `nl-ingest-openapi`, then provisioned live by executing the generated records. Three defects (the third, finding 8, found after publication), quantified design boundaries, and what the description layer cannot supply. All three defect fixes are in the tree; the module's own resolution note is its author's to record. |
| [`aws-sdk-poc`](aws-sdk-poc/) | `published` | A second cloud (AWS Lambda, 85 operations) through a second description format (Smithy → OpenAPI), same adapter, zero modifications — and the two constraints that most bound `gcp-sdk-poc` invert: errors are documented (absent-name examples satisfiable) and bodies are typed (63 observed projections; the corpus carries values). Two defects: a certification-schema regression (fixed, `0b11263`) and synthesized bodied examples that violate their own description's `required` list (open). Rehearsed create→verify→delete→verify-gone against an emulator behind a real SigV4 boundary; real-AWS confirmation is the named next step. |
| [`graphql-poc`](graphql-poc/) | `absorbed` | A description format that is *not* OpenAPI-shaped: GraphQL introspection schemas through a new adapter (`tooling/nl-ingest-graphql`), against three public services. Nothing is spec-derivable, so the whole corpus is observation-gated; the variables encoder is a zero-pull (`render_json` over a `Json` value); one live call per document serves every sibling projection by replay (the rate-limit finding); an all-nullable argument list the service still requires (inexpressible) is resolved by operator binding. Countries 21/21 records, Rick & Morty 27/27, AniList 37 of 142. |
| [`aws-ecosystem-poc`](aws-ecosystem-poc/) | `absorbed` | A whole cloud ecosystem (two regions: network, IAM, ECS on EC2 with Fargate attached, ALB + blue/green, Postgres primary + cross-region replica; globally an accelerator and CloudFront) provisioned, canary-deployed, and torn down entirely through certified records sequenced in `check-plan`-verified plans, over the awsJson adapter it motivated (`tooling/nl-ingest-smithy`, GW19). Names-not-placeholders as the design that keeps plans literal; eleven Cloud Control behaviours no description states; a teardown audited clean by the AWS API and (follow-up, 2026-10-17) by the language's own observation probes, with `RetryAfter`-paced polls and a redaction rule that put the whole vocabulary in the commons. |
| [`integer-tier-poc`](integer-tier-poc/) | `absorbed` | A certified integer tier — Euclid/Bézout, modular arithmetic and exponentiation, inverse and CRT, `isqrt`, deterministic Miller–Rabin for n < 2^63, digits, binomials, and a number-theoretic transform with convolution — 41 pure records authored in surface syntax, composed by address, all certified; after four same-day follow-ups, termination and complexity SOUND on all 41 (the NTT chain at `O(n log n)` through shrinking/preserving-callee facts re-derived over the linked view), 18 contracts and 13 laws proved — `lcm`'s through `gcd`'s certified contract, `ext_gcd`'s Bézout identity over a tuple result — and 31 laws held generatively across magnitudes. The gauntlet found eight evaluator/checker defects (silent i128 wrap, int comparison through a float, stack-overflow abort, unreadable `self(…)` laws, `nat`-erasing generation, …), all fixed in-tree, and forced linked callee folding, numeric induction, callee contracts as axioms, and SMT datatypes into the validator. |
| [`rational-tier-poc`](rational-tier-poc/) | `published` | An exact rational tier — `(int, int)` pairs normalized through the integer tier's `gcd` applied **by content address across the module boundary**: normalize, negation, reciprocal, add/sub/mul, equality and order by cross-multiplication, floor, and five algebra checkers — 14 records, all certified; every contract SOUND (11), among them **lowest terms** as a post on `normalize` that applies the other module's `gcd` by address inside the predicate and proves through its greatest-divisor law, and value-preserving identities on the arithmetic records derived from `normalize`'s own proved cross-multiplication law (contracts compose across modules exactly as within one, including a contract that applies a callee); 21 of 22 laws proved over seven rounds (round 6, 2026-10-31: the first half of uniqueness — equal values have cross-equal normal forms — as a certified law, and `rat_eq`'s decision as a post, 12 contracts SOUND; round 7, the same day: **uniqueness of lowest terms PROVED in the tool** — the hand-measurement had said every step closes alone and no one-shot query does, so the prover gained a **staged** rung (contract-law instances' conclusions established by refuting their other disjuncts one at a time, then finals over abstracted witnesses) and a **witness-callee** lookup that brings the integer tier's `ext_gcd` — applied by nothing here — to the goal's `gcd` terms through its link post `fst(ext_gcd) = gcd`; the checker's law closes with 100 conclusions over 2 branches), the one UNKNOWN left additive associativity, which needs that uniqueness as a law on `normalize` that callers inherit — recorded, not built. The gauntlet forced the product-witness rung into the prover and found three validator defects, all fixed in-tree (a callee's contract instance stated outside a pattern's scope; a callee declared after the axioms that mention it; a caller's contract set that did not close over the callees its callees' contracts apply), plus one fold limit (complexity through product arguments). |
| [`gaussian-tier-poc`](gaussian-tier-poc/) | `published` | A Gaussian-integer tier — `(int, int)` pairs `a + b·i`: add/sub/neg/conj/mul, the norm `a² + b²`, units, nearest-integer division with the Euclidean remainder, and Euclid's algorithm on the norm — 14 records, all certified; **19 of 19 contracts SOUND and 23 of 24 laws PROVED, every one nonlinear** (the norm multiplicative — Brahmagupta–Fibonacci, degree 4 — conjugation multiplicative, units on the axes), including the module's headline theorem `N(z − q·w) < N(w)` as a post on the remainder, which needed a new prover rung: the **ground rung** (the instances-only certificate in `QF_NIA` with callee applications as constants — z3's nonlinear engine decides in under a second what the combination solver did not in 90 s). Three rounds found three more validator defects (closed laws refused as "not a `forall`"; a record's own posts and laws applying a callee its body does not; the refinement checker hiding the first-order refusal), all fixed in-tree. Its follow-up (2026-10-31) gave the termination checker **a measure that is a callee of several positions**: the Gaussian `gcd` descends on the norm `N(c, d)` by the remainder's certified post `N(z − q·w) < N(w)`, the pre discharged from the guard by the solver — termination SOUND on all 14, and the prover inducts on that measure: **25 of 25 laws PROVED**, including a genuine strong induction on `N(c, d)` (the gcd is nonzero unless both inputs are). |
