# Efficiency report — measured against production (2026-07-12)

The project's headline — assemble-from-the-commons beats write-from-scratch on context
cost, and verification is *checked*, not re-derived — measured for the first time, against
the live production node (Arca, 321 function records), with no model API calls. Tokens are
exact BPE counts under the Qwen2.5 tokenizer (the reference-checkpoint model family).
Produced by [`measure_efficiency.py`](measure_efficiency.py); the coverage lever by
[`cert_sweep.py`](cert_sweep.py). Re-run both to reproduce — every input is the public
node.

## 1. Discovery cost (production round-trips, by intent)

| intent | matches | hashes | summary | full records | budget-capped (2k) |
|---|---|---|---|---|---|
| `arithmetic` | 80 | 4,809 | 17,550 | 35,717 | 3,865 (top 18) |
| `string` | 28 | 1,718 | 6,426 | 11,541 | 3,825 (top 17) |
| `io/network/http` | 26 | 1,589 | 6,596 | 10,910 | 3,560 (top 14) |
| `parse` | 18 | 1,105 | 4,173 | 7,142 | 3,661 (top 16) |
| `query/pages` | 1 | 78 | 310 | 693 | 344 |

Resolving every `arithmetic` candidate to a full record costs 35.7k tokens; the summary
projection halves it; the token-budget cap makes it **O(budget), not O(matches)** — 3.9k
tokens for the 18 best-ranked, a **9.2× cut** on the worst intent measured. This is the
"discovery cost" open problem's mitigation working as designed on production data.

## 2. Assembly vs authoring (the GW15 pagination chain, 4 records)

| context held | tokens |
|---|---|
| assemble, first use (budgeted discovery + decision summary + address + served cert) | 1,135 |
| assemble, each reuse (summary + address under a cached trust verdict) | **256** |
| author from scratch (all 4 signatures + surface bodies, perfect first shot) | 878 |
| runtime closure (what the *runtime* — not the agent — fetches and hash-verifies) | 3,700 |

**The honest reading cuts both ways.** First-time assembly (1,135) costs *more* context
than holding this chain's finished sources (878) — the surface dialect is compact and a
4-record closure is small, so discovery + cert overhead dominates; the commons does not
win on first contact at this corpus size. It wins on **reuse (3.4×)** — the address is the
program — and the authoring figure is a *floor*: it assumes a perfect first shot with no
reference material and no verification, while the measured write accuracy of the pinned
reference models is 93–98% (see `eval/REFERENCE_CHECKPOINT.md`), i.e. real authoring pays
retries plus a certify cycle per attempt.

## 3. Verification asymmetry (40 certified records)

Reading the served certificate costs **494 tokens** (mean) vs **647 tokens** to fetch what
re-derivation needs (record + body) — a modest 1.31× on tokens. The real asymmetry is
compute and tooling: re-deriving runs the full verified-by-default pass (typecheck,
effects, refinements, termination, complexity, property proofs — mean 0.01–0.03 s on the
simple sampled records, unboundedly more for proof-carrying ones, plus a local validator
and solver), while the certificate checks with one signature + hash verification. A
consumer without the toolchain can still *trust-check* (`certified --subject fn_…`) —
that path has no re-derivation equivalent at any token price.

## 4. Composition success (assembled pipelines)

Of 400 type-plausible ordered pairs of certified functions, **400 compose** (100%) with
sound derived metadata (type threading, effect/capability union, termination conjunction,
coarse complexity). **0 pairs achieve *precise* complexity composition** — no record on
the node carries the v0.3 `cost` metadata yet (the corpus rows do; the production records
predate it). That is the named, honest gap this metric leaves open.

*(Closed 2026-07-12 by `cost_sweep.py` — the cert-sweep move applied to `cost`: for every
pure, body-hosted, un-costed v0.2 record it infers the time class from `check-complexity`'s
own structural analysis, finds the tightest `output_size` the checker verifies, and
publishes a superseding costed record + signed cert. One run costed **133 records** (99
honest refusals: higher-order/opaque time or unestablished output class — the fold/map
family; effectful records are out of scope on purpose, their time is the effect's). The
metric now reports a dedicated costed-pair subsample — the first-400-pairs cap walks
enumeration order and had left the newly-costed tail entirely unsampled, reading 0 while
costed pairs composed precisely: of **100 costed pairs, 100 compose and 98 on the precise
basis** (`cost-basis precise (output-size substitution)`); the 2 coarse fallbacks are
stages whose established output class cannot re-express the downstream measure — the
substitution rule refusing, not failing.)*

## 5. Certification coverage — measured, then moved

| | before sweep | after `cert_sweep.py` |
|---|---|---|
| total (321 fns) | 22.4% | **81.3%** |
| v0.2 structured (264) | 27.3% | **98.9%** (261/264) |
| v0.1 surface-typed (57) | 0% | 0% |

The baseline measurement exposed the gap: certifications had only ever been seeded
alongside specific workflows, never swept across the node's full holdings. The sweep
certified **189 records from the node's own hosted bodies** (fetch closure → hash-verify →
`certify --sign` → publish through the verify-then-store gate) in one run. The named
residuals: 3 v0.2 records whose bodies were never hosted (early samples — `clamp`,
`sign`, `abs_diff`; since investigated, 2026-07-12: the bodies predate the
host-the-body-with-the-record invariant and are unrecoverable — absent from every local
corpus vintage and from all 54 historical `corpus.jsonl` git blobs — so these three stay
permanently uncertifiable in the append-only store; newer certified twins of all three
names are hosted, so no capability is lost), and the 57 v0.1 surface-typed bulk-ingested records, which are not
certifiable as-is — they need `--v2` re-ingestion (structured types + executable bodies),
which is exactly the planned ingestion-sweep work. *(Since measured and dispatched — the
sweep's third increment, 2026-07-12: types-from-stubs + examples-by-execution upgraded the
tier's one certifiable function (`colorsys.rgb_to_yiq`, certified, superseding its v0.1
twin on the node) and itemized the remaining 56 as boundary, not backlog: 10 body-no-type,
10 doctest-no-body, 36 no-body — see `spec/expressiveness.md`.)*

## Verdict

Three claims now have numbers: discovery is budget-boundable (9.2× cut, O(budget)),
composition metadata propagates at a 100% rate on type-plausible pairs, and certification
coverage is 98.9% of certifiable holdings. One claim came back *against* the headline at
current scale — first-contact assembly costs ~1.3× a perfect author's context on a small
chain — with the honest qualifiers that reuse inverts it (3.4×) and perfect authorship is
a floor no measured model achieves. The two named gaps: `cost` metadata absent from
production records (precise composition unrealized — since closed by `cost_sweep.py`, see
§4's addendum: 133 records costed, 98/100 costed pairs compose on the precise basis), and
the v0.1 tier awaiting `--v2` re-ingestion (since dispatched — see the coverage section's
addendum: 1 upgraded, 56 measured as boundary).
