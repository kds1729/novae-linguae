# A second cloud, a second description format, the same adapter

- **Status:** absorbed (2026-08-20). Both defects it reports are fixed in-tree (`e27b281` —
  [finding 1](findings.md#1-certifys-schema-check-emits-verdicts-the-certification-schema-forbids);
  `2099326` — [finding 3](findings.md#3-a-bodied-operations-synthesized-worked-example-violates-the-descriptions-own-schema)),
  findings 2, 4, 5 and 6 are recorded measurements,
  [finding 7](findings.md#7-the-live-service-serializes-absent-members-as-explicit-null--and-the-description-does-not-admit-it)
  is resolved (null-as-absence admitted for optional members, refused for required ones), and
  **the live confirmation against real AWS ran 2026-08-20: the loop closed 201 → 200 → 204 →
  404 and the slice close published the first real-cloud AWS artifacts to the commons** — see
  [Live confirmation](#live-confirmation-2026-08-20); everything below it is
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
(by `fn_ref`, under host-scoped grants, traces captured and replaying grantless offline) — first
against a local emulator through a real SigV4-signing boundary, and since 2026-08-20
[against real AWS](#live-confirmation-2026-08-20), where the same loop closed 201 → 200 → 204 → 404.

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
| [`findings.md`](findings.md) | The seven findings, measurements separated from conclusions |
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

## Live confirmation (2026-08-20)

The named next step ran, and the provenance table proved its worth: the pipeline was reproduced
from it on a second machine — the Smithy model fetched at `bedddbec` (sha256 verified,
`f70ac72f4f028bac…`), smithy-cli 1.73.0 reinstalled from its release archive, the two
description transforms applied, and the corpus regenerated with the adapter at `5aaffda`:
**84 of 85 operations compiled, with the same single refusal**
(`GetDurableExecutionState`, documents no 404). The loop then ran against **real AWS Lambda**
(us-east-1), through a local SigV4 signing proxy holding an IAM user scoped to read-wide /
write-`nl-*`-only:

| step | record | answered | trace |
|---|---|---|---|
| create `nl-live-1` | `createfunction` | **201** | `trc_eeba484d…` |
| verify | `getfunction` | **200** | `trc_c4e642f8…` |
| delete | `deletefunction` | **204** | `trc_f6422bd2…` |
| verify-gone | `getfunction` | **404** | `trc_537e218b…` |

Each step is a generated record applied by `fn_ref` under host-scoped grants
(`net.write@127.0.0.1` / `net.read@127.0.0.1`). The create trace replays to its 201 with the
proxy killed — no grants, no credential, no service — and `ListFunctions` answers 0 after
teardown. Record-side there is still no credential anywhere: the SigV4 identity lived entirely
at the proxy, now exercised against the real signer rather than a rehearsal one (finding 5's
split, confirmed). And finding 4's caveat resolves in the right direction: moto answered this
same loop 201/200/204/404 and the real service agrees — an agreement that was only knowable by
running it.

**The slice close (same day): the first real-cloud AWS artifacts in the commons.** The
adapter at head live-gated `ListFunctions` + `GetAccountSettings` against the real service,
run against the *deliberately empty* account so no observed value or trace can carry an ARN
— **7 records (2 base + 5 observed projections), 2 deduped traces, and 7 signed
certifications, all through Arca's verify-then-store gate** (which also exercises the
finding-1 fix in production: freshly signed certifications store again). The verified loop
closed from the node: precise-tag query `parse/get-account-settings-accountusage` → **1
match**, fetched hash-verified, certified, applied under `net.read@127.0.0.1` → `Just
{FunctionCount: 0, TotalCodeSize: 0}` → CONFIRMED, observed assert `msg_faff8b84…`
published — and the loop's live trace hashed to `trc_486dcba7…`, **the very address the
ingestion observation published** (byte-identical document, deduped across events).
Grantless, secretless, proxy-dead third-party `verify-claim` by address: CONFIRMED. The
constructive half ran too, locally: `--observe-arg` bindings at an operator-created
`nl-live-2` materialised **43 records** (`getfunctionconfigurationruntime` → `Just
"python3.12"`, trace-attached, replaying offline) — deliberately **not** published: observed
Lambda documents embed ARNs, and the account identity is the operator's to publish, so it is
withheld (the same division as finding 5 — what binds a record to an environment stays on
the operator's side of the boundary). En route the gate surfaced
[finding 7](findings.md#7-the-live-service-serializes-absent-members-as-explicit-null--and-the-description-does-not-admit-it):
the real service's explicit-`null` serialization of absent members refuses every
whole-document projection whose schema says the member is a typed optional.

## The world-state close (2026-08-20, same day)

[`spec/world-state.md`](../../spec/world-state.md)'s machinery — built and demonstrated against
the fake service's note-on-item dependency — met a real dependency surface: **Lambda's
`PutFunctionConcurrency` answers 404 unless the function exists** (measured live at an absent
name before anything was declared). Four records were re-issued with world contracts (the
cost_sweep pattern — bodies and examples unchanged, superseding addresses): the create *ensures*
`function(base, body.FunctionName)` exists — **the finding-9 `body-field` key's first real-cloud
discharge**, grounding the resource from the literal create body exactly as the 131/131
measurement said it must — the delete ensures it absent, and both concurrency operations require
the function and ensure the concurrency resource. `GetFunction` deliberately carries no contract
(a total probe is meaningful on both states).

Three plans, three verdicts, and the live runs agree with all of them:

| plan | `check-plan` | run live against real AWS |
|---|---|---|
| concurrency with no create | **UNVERIFIABLE** — "nothing establishes it" | not run — that is the point |
| create → put-concurrency → delete-concurrency → delete | **PLAN-SOUND** (the body-field ensures discharges both requires) | **201 → 200 → 204 → 204** |
| create → delete → put-concurrency | **REJECTED** at step 3: "it is absent (step 2 (deletefunction))" | run anyway: **201 → 204 → 404** — the predicted status, step, and cause |

The check preceded the effect on a real cloud: the symbolic verdict, computed from declarations
alone, predicted the live outcome exactly. The re-issued records, plans, and step traces stay
outside the repository — create bodies and plan data embed ARNs, which are the operator's
(the same boundary the slice close drew).

This surface then drove two rungs [`spec/world-state.md`](../../spec/world-state.md) had held
as deliberately-out. **Plans became commons artifacts** (`plan_…`, `2e2507a`): the OQ4 worked
lifecycle plan published through Arca's gate as `plan_e4cf79e5…` and re-decided PLAN-SOUND
from the live node by address. And **observation probes** closed the testimony gap this
module's dependency made concrete: a fourth plan — put-concurrency resting on the stated
*assumption* that `function(base, nl-ws-1)` exists — is symbolically sound on that testimony,
and `check-plan --probe function=fn_…` decided it against the real world three ways in one
afternoon: **404 → PROBE-REFUTED** (the plan rests on false testimony and must not run — before
its write could fail), a deactivated credential's **403 → INCONCLUSIVE** (an access fact never
mints a world verdict; the assumption stays testimony), and after a real create, **200 →
OBSERVED** ("every assumption confirmed by observation") — whereupon the licensed plan ran and
its one step answered exactly its ensured 200.

## Reproducing

Finding 3 is fully hermetic (see [`findings.md`](findings.md) § Reproducing). The pipeline lives
outside this repository (it needs `smithy-cli` and, for the rehearsal, `moto`); the provenance
table above pins every input, and the live confirmation above is its measured reproduction. The
live step needs an AWS account with a Lambda execution role and a SigV4 signing boundary; no
finding depends on it.
