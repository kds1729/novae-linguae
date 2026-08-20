# A second cloud, a second description format, the same adapter

- **Status:** published. Both defects it reports are fixed (`e27b281` — [finding 1](findings.md#1-certifys-schema-check-emits-verdicts-the-certification-schema-forbids);
  `2099326` — [finding 3](findings.md#3-a-bodied-operations-synthesized-worked-example-violates-the-descriptions-own-schema)).
  The live confirmation against real AWS is the module's named next step; everything below it is
  emulator-rehearsed and hermetically reproducible.
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-08-20, last updated 2026-08-20
- **Scope:** `tooling/nl-ingest-openapi` (exercised unmodified against a second vendor's format
  chain; one open defect reported); `spec/certification.schema.json` (finding 1, fixed); extends
  `evolution/gcp-sdk-poc` — its findings 3, 7 and 11 get cross-vendor generalisation data here.
- **Provenance:** upstream commit `e27b281`; Smithy model `aws/api-models-aws` commit `bedddbec`
  (2026-08-19), `models/lambda/service/2015-03-31/lambda-2015-03-31.json`
  (sha256 `f70ac72f4f028bac…`); `smithy-cli` **1.73.0** (openapi plugin, with
  smithy-aws-traits / -aws-iam-traits / -aws-endpoints / -waiters / -rules-engine /
  -smoke-test-traits / -aws-smoke-test-model / -aws-apigateway-openapi 1.73.0); **moto 5.2.2**
  (`moto[server]`, pip) as the rehearsal service; run date 2026-08-19.
- **Resolution:** module committed to `main` directly (solo-maintainer repo). Finding 1's fix is
  `e27b281`. Findings 2, 4, 5 and 6 are measurements, not requests. Finding 3's suggested
  resolution was accepted by the maintainer and fixed in `2099326` — the hermetic repro below now
  refuses with the reason.

## Summary

AWS Lambda — **85 operations** — was compiled into Nova Lingua records by `nl-ingest-openapi`
from a pipeline with *no Google-shaped step in it*: AWS publishes Smithy JSON AST models, so the
chain is `Smithy model → smithy-cli openapi projection → normalize → nl-ingest-openapi`.
**The adapter needed zero modification** for the second vendor and second format family, and a
function was provisioned create → verify → delete → verify-gone by executing the generated records
(by `fn_ref`, under host-scoped grants, traces captured and replaying grantless offline) — against
a local emulator through a real SigV4-signing boundary, pending the same loop against real AWS.

Where `gcp-sdk-poc` was mostly negative space, this description format inverts the two
constraints that most bound it:

- **The errors are documented.** 76 of 85 operations document a 404 and all 85 document at least
  one non-2xx (Discovery: 0 of 1,164). The absent-name convention — unsatisfiable by construction
  there — is *satisfiable* here, and the finding-11 fix's refusal half fired exactly once,
  correctly, on the one operation that documents no 404.
- **The bodies are typed.** 66 operations declare `application/json` 2xx bodies (Discovery
  converts to `*/*` everywhere), so projections license: the live-gated run produced **63 observed
  field projections** from two `--observe-arg` bindings plus the constructible list operations.
  This corpus carries values — the "nodes without edges" boundary (gcp finding 3) does not bind.

The negative space that remains is sharp and new: **a bodied operation's synthesized worked
example (`{}`) violates the description's own `required` list** on 16 of 33 requestBody
operations, and still certifies — finding 11's family, one verb class over
([finding 3](findings.md#3-a-bodied-operations-synthesized-worked-example-violates-the-descriptions-own-schema),
hermetic fixture in [`repro/`](repro/)). And AWS moves *both* auth and endpoint from data to
computation (SigV4, endpoint rules), which forces the operator-resupply split gcp finding 7 only
gestured at ([finding 5](findings.md#5-aws-moves-auth-and-endpoint-from-data-to-computation-the-operator-resupplies-both)).

Standing the commons node up for this corpus also caught a fresh regression in the tree: the
finding-12 fix gave `certify` a `schema` check whose verdicts the certification schema's own enum
forbade, making every newly signed certification unpublishable — the producer/admitting-authority
disagreement it was built to close, reintroduced one layer up. Fixed in `e27b281`
([finding 1](findings.md#1-certifys-schema-check-emits-verdicts-the-certification-schema-forbids)).

## Contents

| file | what |
|---|---|
| [`findings.md`](findings.md) | The six findings, measurements separated from conclusions |
| [`repro/`](repro/) | Hermetic fixture for finding 3 — no vendor, no converter, no service, no credentials |

## What worked well

- **The description layer is genuinely format-plural.** Discovery→Swagger→OpenAPI (gcp) and
  Smithy→OpenAPI (here) land in the same adapter unchanged, and their differences surface as
  *measured properties of the descriptions*, not as code paths.
- **Both halves of the finding-11 fix fired correctly on first contact** — the constructive half
  (`--observe-arg` supplying a reachable documented success) and the refusal half (the one
  operation with no documented 404).
- **The secret story got simpler, not harder.** Record-side there is *no* credential at all: the
  SigV4 identity lives entirely at the operator's signing boundary, records and traces carry
  nothing, and every captured trace replays with no grants, no proxy, no service.

## Reproducing

Finding 3 is fully hermetic (see [`findings.md`](findings.md) § Reproducing). The pipeline lives
outside this repository (it needs `smithy-cli` and, for the rehearsal, `moto`); the provenance
table above pins every input. The live provisioning step needs an AWS account; no finding depends
on it.
