# A whole cloud API through the description layer

- **Status:** accepted — findings merged. [Proposal 01](proposals/01-request-content-type.md) remains
  `proposed` and resolves separately; this module reaches `absorbed` if and when it is decided.
- **Author:** Keith Sprochi <ksprochi@elementalmachines.com>
- **Dates:** first published 2026-07-30, last updated 2026-07-31
- **Scope:** `tooling/nl-ingest-openapi`; touches on `tooling/commons-node` retrieval and on
  `spec/expressiveness.md` GW7/GW10/GW16 as the results this extends
- **Provenance:** upstream commit `76fc6ba` (2026-07-15); Google Cloud Storage Discovery document
  `storage:v1` revision **20260719**; `google-discovery-to-swagger` **2.1.0**;
  `swagger2openapi` **7.0.8**; run date 2026-07-26
- **Resolution:** module merged in #2. Defect 2 fixed in #1
  (`2659c09`). Defect 1 open, carried as proposal 01.

## Summary

Google Cloud Storage v1 — **all 81 operations** — was compiled into Nova Lingua records by
`nl-ingest-openapi` and loaded into a local commons node, and a bucket was then provisioned
create → verify → delete against live GCS by executing those records through `nl-validator run`.
Nothing was hand-authored, and **no modification to this repository was required**: the two
transforms the pipeline applies are to the *description*, not to the adapter or the language.

So the description-layer bet holds at cloud scale. The useful content of this module is the negative
space, and it is uncomfortable in one specific way: **the generated corpus cannot express a plan.**
Every leaf record projects `.status` and discards `.body`, because the projection constructibility
rule admits 1 of 81 operations here. A set of operations returning `int` has nodes and no edges, so
no value can flow from one call into the next regardless of how many operations are ingested. For a
consumer whose goal is composing multi-resource work, that is the binding constraint — and it is a
constraint on where a worked example may come from, not on anything the runtime cannot do.

Two defects surfaced along the way, one of them silent for days.

## Contents

| file | what |
|---|---|
| [`findings.md`](findings.md) | The seven findings, each with the measurement behind it |
| [`proposals/01-request-content-type.md`](proposals/01-request-content-type.md) | Emit the declared request `Content-Type` — needs a decision on record re-addressing |

## Defects reported

1. **The request `Content-Type` is dropped** — an operation with a `requestBody` compiles to an
   `http` call whose header argument is `map_empty`, so the record is rejected by any service that
   requires the header, while still reporting `certify=OK`. **31 of 81** operations affected, which
   is every write operation in the API. Reproduces on hand-authored OpenAPI 3 with no converter
   involved. See [proposal 01](proposals/01-request-content-type.md).

2. **A non-JSON 2xx response body was declined silently** — no projection *and* no note, so a
   consumer could not distinguish "the description promised no body" from "the description promised
   one the adapter declined to carry." The Cloud Storage run emitted **877** notes, every one about
   an optional query parameter, and **zero** about the **69** response bodies it declined. The
   trigger is the media range `*/*`, which is what *any* Swagger 2.0 description without `produces`
   converts to — so the exposed class is far wider than one vendor.
   **Fixed in #1** (`2659c09`) — diagnostics only, no artifact changes. The adapter now names the
   media type it declined and why.

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

Where the next person should start, most consequential first.

1. **Can a projection's worked example come from an observation rather than the spec?** The
   constructibility rule — bodyless `GET`, no path parameters — is sound in its reasoning and
   admits 1 of 81 real operations. The live gate *already* sources examples from observations for
   schema-derived projections, so the machinery exists; the question is whether the faithfulness
   contract can accommodate it more widely. This is the gate on dataflow, and therefore on
   composition.
2. **Should the live observation gate be usable read-only?** It cannot be used at all here, because
   it would create real resources during ingestion for mutating verbs. `GET`/`HEAD` are `net.read`
   and create nothing, so a read-only gate would be safe and would materialise projections wherever
   question 1 permits them.
3. **Composite-level intent tags and retrieval.** Leaf tags describe a *call*, and exact
   `name_hint_prefix` query resolves one when the caller knows its name. Finding a prior *assembly*
   means searching for an outcome the caller cannot name, which the stdlib lexical embedder does not
   serve. Without this, accumulated designs are write-only.
4. **Refinements over world state.** "Requires a VPC, guarantees a subnet" is what would let an agent
   check a plan before performing any effect. Distinct from argument/result refinements, and the one
   genuinely new capability on this list rather than a gap — every record here carries
   `refinements: []` because descriptions carry no pre/postconditions.

## Reproducing

The pipeline lives outside this repository (it needs `node` for the Discovery→OpenAPI converters).
Findings 1–3 need no cloud credentials and no network; only the live provisioning step does.

See [`findings.md`](findings.md) § Reproducing for the commands.
