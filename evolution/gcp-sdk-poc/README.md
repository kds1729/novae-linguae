# A whole cloud API through the description layer

- **Status:** published — back from `absorbed`. The two original defects are fixed and
  [proposal 01](proposals/01-request-content-type.md) is `accepted` and implemented, but
  [finding 8](findings.md#8-an-optional-field-projection-materialises-off-a-failed-call) is a new
  defect found after publication and is outstanding, so the module no longer qualifies as absorbed.
- **Author:** Keith Sprochi <ksprochi@elementalmachines.com>
- **Dates:** first published 2026-07-30, last updated 2026-08-01
- **Scope:** `tooling/nl-ingest-openapi`; touches on `tooling/commons-node` retrieval and on
  `spec/expressiveness.md` GW7/GW10/GW16 as the results this extends
- **Provenance:** upstream commit `76fc6ba` (2026-07-15); Google Cloud Storage Discovery document
  `storage:v1` revision **20260719**; `google-discovery-to-swagger` **2.1.0**;
  `swagger2openapi` **7.0.8**; run date 2026-07-26
- **Resolution:** module merged in #2. Defect 2 fixed in #1 (`2659c09`); defect 1 fixed by
  accepting [proposal 01](proposals/01-request-content-type.md). The module's **open questions**
  remain open — they are pointers for the next author, not outstanding proposals.

## Summary

Google Cloud Storage v1 — **all 81 operations** — was compiled into Nova Lingua records by
`nl-ingest-openapi` and loaded into a local commons node, and a bucket was then provisioned
create → verify → delete against live GCS by executing those records through `nl-validator run`.
Nothing was hand-authored, and **no modification to this repository was required**: the one transform
the pipeline applies is to the *description*, not to the adapter or the language.

So the description-layer bet holds at cloud scale. The useful content of this module is the negative
space, and it is uncomfortable in one specific way: **the generated corpus cannot express a plan.**
Every leaf record projects `.status` and discards `.body`. The run licensed **zero** body projections
— every response arrives as `*/*` and fails the media-type check (finding 2) — and even once that is
compensated for, the constructibility rule would admit only **1 of 81** operations (finding 3). A set
of operations returning `int` has nodes and no edges, so no value can flow from one call into the
next regardless of how many operations are ingested. For a consumer whose goal is composing
multi-resource work, that is the binding constraint — and it is a constraint on where a worked example
may come from, not on anything the runtime cannot do.

Two defects surfaced along the way, one of them silent for days.

## Contents

| file | what |
|---|---|
| [`findings.md`](findings.md) | The eleven findings, each with the measurement behind it |
| [`repro/`](repro/) | Hermetic fixtures — no cloud account, no credentials, no vendor input |
| [`proposals/01-request-content-type.md`](proposals/01-request-content-type.md) | Emit the declared request `Content-Type` — **accepted**, applied, and its three questions answered |

## Defects reported

1. **The request `Content-Type` is dropped** — an operation with a `requestBody` compiles to an
   `http` call whose header argument is `map_empty`, so the record is rejected by any service that
   requires the header, while still reporting `certify=OK`. **31 of 81** operations affected, which
   is every write operation in the API. Reproduces on hand-authored OpenAPI 3 with no converter
   involved. **Fixed** — [proposal 01](proposals/01-request-content-type.md) accepted and applied;
   the adapter now emits the declared media type, and notes-and-omits when the description declares
   more than one.

2. **A non-JSON 2xx response body was declined silently** — no projection *and* no note, so a
   consumer could not distinguish "the description promised no body" from "the description promised
   one the adapter declined to carry." The Cloud Storage run emitted **877** notes, every one about
   an optional query parameter, and **zero** about the **69** response bodies it declined. The
   trigger is the media range `*/*`, which is what *any* Swagger 2.0 description without `produces`
   converts to — so the exposed class is far wider than one vendor.
   **Fixed in #1** (`2659c09`) — diagnostics only, no artifact changes. The adapter now names the
   media type it declined and why.

3. **An optional field projection materialises off a FAILED call** — when the observation gate's call
   does not yield the declared 2xx document, the whole-document projection correctly refuses while
   every *optional* field projection falls through and materialises with a `None` worked example,
   reported `live=OBSERVED+schema-checked certify=OK examples=PASS`. One run, one response, opposite
   verdicts. The guard that would catch it is `required_field`, and Google Discovery emits no
   `required` at all — **0 of 34** schemas here declare one — so it is dead code for this entire
   class of description. An expired token would silently mint a `None`-valued, certified projection
   for every field of every operation in a corpus. **Open**; found after publication, reproduced
   against `c482645`, hermetic fixture in [`repro/`](repro/). See
   [finding 8](findings.md#8-an-optional-field-projection-materialises-off-a-failed-call).

## What worked well

Recorded because it is what most shaped the downstream work, and it is signal a defect report cannot
carry:

- **Effect enforcement is not metadata.** `run` grants exactly the record's declared effects, and a
  record that under-declares fails its own examples.
- **Grant scoping composes usefully.** `net.write@host/path` matched segment-aligned, plus
  `--grant-certified` gating a grant on the target's certification under local policy, gave
  per-delegation least privilege with an auditable transcript of which gates opened. This is the
  property that made it reasonable to hand execution to an agent without supervising each call.
- **Secret placeholders behaved exactly as documented.** `{{secret:Bearer}}` substituted only at the
  effect boundary; the token appears in no record, no trace, and no published artifact, and replay
  needs no credentials.

## Open questions

The four this module first raised have all been answered in-tree. Recorded here with what exercising
each against the corpus then showed — the answers work, and each surfaced the next constraint.

1. ~~Can a projection's worked example come from an observation rather than the spec?~~ and
   2. ~~Should the live observation gate be usable read-only?~~ — **answered by `--observe-arg`**
   (operator-supplied observation arguments, read-only by rule). Exercised: `storage.buckets.get`,
   excluded by the constructibility rule since day one, now yields **40 records** — one status, one
   whole-document projection, 38 typed field projections, each trace-attached and offline-replayable
   (`getName -> Just("em-devops-…")`, `getLocation -> Just("US")`). The corpus can carry values, not
   just `int`s. Keeping the operator's values out of the description and in the invocation is the
   right split; an earlier local spike that declared them in the description was worse, because it
   would bake one operator's environment into a shared artifact.
3. ~~Composite-level intent tags and retrieval.~~ — **answered by `assemble`'s derived discovery
   metadata and the node's `/v0/records/{hash}/derivations`.** Not reachable from this corpus yet:
   `/derivations` needs a composite to exist, and [finding 10](findings.md#10-assemble-cannot-admit-any-record-from-this-corpus)
   is why none can be assembled.
4. ~~Refinements over world state.~~ — **answered by `check-plan` and `spec/world-state.md`.**
   Exercised: it correctly `REJECTED` a use-after-delete, naming the step, the resource, and the
   prior step whose `ensures` contradicted it. [Finding 9](findings.md#9-world-refinements-cannot-key-a-resource-the-request-body-names)
   is what exercising it surfaced — the create half of every lifecycle cannot declare its `ensures`,
   because the new resource is named in the request body.

### Still open

- **Matching an effectful candidate by replayed evidence** rather than live execution
  ([finding 10](findings.md#10-assemble-cannot-admit-any-record-from-this-corpus)). Without it,
  `assemble` admits nothing from any service-derived corpus — 121 of 121 records here are effectful.
- **Keying a world resource the request body names**
  ([finding 9](findings.md#9-world-refinements-cannot-key-a-resource-the-request-body-names)) — the
  driver the v0.1 world-state spec says richer vocabulary should be earned by.
- **The finding 8 defect**, unfixed at `ca24c88`.

## Reproducing

The pipeline lives outside this repository (it needs `node` for the Discovery→OpenAPI converters).
Findings 1–3 need no cloud credentials and no network; only the live provisioning step does.

See [`findings.md`](findings.md) § Reproducing for the commands.
