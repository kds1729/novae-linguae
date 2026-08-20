# Findings — AWS Lambda through `nl-ingest-openapi`

Companion to [`README.md`](README.md). Per [`evolution/README.md`](../README.md), each finding
states what was observed, then what the author concludes; a reader may reject every conclusion
and keep every number.

## Provenance

| input | pinned value |
|---|---|
| Smithy model | `aws/api-models-aws` commit `bedddbec` (2026-08-19), `models/lambda/service/2015-03-31/lambda-2015-03-31.json`, sha256 `f70ac72f4f028bac…` |
| Smithy → OpenAPI 3 | `smithy-cli` **1.73.0**, `openapi` plugin (protocol `aws.protocols#restJson1`), with the 1.73.0 trait/dependency set named in the README |
| description transforms | strip `aws.auth#sigv4` (finding 5); set `servers[0].url` to the operator's signing entry point (finding 5) |
| rehearsal service | **moto 5.2.2** (`moto[server]`, pip), behind a local SigV4-signing proxy |
| upstream commit | `5aaffda` for the ingest runs; `e27b281` (this module's finding 1 fix) for the node loading |
| run date | 2026-08-19 |

AWS publishes Smithy JSON AST models rather than OpenAPI 3, so the pipeline is
`Smithy model → smithy-cli openapi projection → normalize → nl-ingest-openapi → commons node`.
No Google-shaped converter is involved; this is a second, independent route into the adapter.

## What was measured

| quantity | value |
|---|---|
| operations in the converted description | 85 |
| operations compiled + certified (hermetic run) | 84 |
| operations refused | 1 — `GetDurableExecutionState`, honestly, with the reason (documents no 404) |
| operations documenting a 404 | **76 of 85** |
| operations documenting at least one non-2xx | **85 of 85** (400 on 84, 500 on 85) |
| operations with a `requestBody` | 33 |
| …whose schema declares `required` fields | **16** |
| 2xx responses declaring `application/json` | 66 (plus 1 `application/octet-stream`, 1 eventstream) |
| live-gated run: records written | 145 |
| …certified | **104** = 41 status records live=PASS + **63 observed field projections** |
| …carrying a live-gate failure | 41 (classified in findings 3 and 4) |
| hermetic artifacts stored by a commons node, refused | 168, 0 |
| modifications to the adapter required | **0** |

The rehearsed loop, each step a generated record applied by `fn_ref` under host-scoped grants
(`net.write@127.0.0.1` / `net.read@127.0.0.1`), one trace artifact per step:
create → **201**, verify → **200**, delete → **204**, verify-gone → **404**; the create trace
replays with no grants, no proxy, and no service running.

---

## 1. `certify`'s schema check emits verdicts the certification schema forbids

Found while standing a commons node up for this corpus, at upstream `5aaffda`; the node's own
suite caught it (3 failures).

**Measured.** `certify.rs` (since `25fe525`, the finding-12 fix in `gcp-sdk-poc`) prepends a
`schema` check whose verdicts are `VALID` / `SCHEMA-INVALID`. The verdict enum in
`spec/certification.schema.json` contained neither, so the node's verify-then-store gate answered
every freshly signed certification with:

```
422 schema_invalid: at /checks/0/verdict: "VALID" is not one of ["WELL-TYPED", …]
```

**Concluded.** The producer and the admitting authority disagreeing about what a valid artifact
is was gcp finding 12's exact family — and this instance was *introduced by that finding's fix*,
one layer up (the record schema gained a check; the certification schema never learned the
check's vocabulary). A gate and its producer sharing a vocabulary is a single fact that lived in
two places.

**Resolved** in `e27b281`: the enum admits both verdicts. All 173 node tests and the validator
suite pass; the 168-artifact load reported `stored=168 skipped=0 failed=0`.

## 2. A Smithy-derived description documents its errors — the absent-name convention is satisfiable

**Measured.** 76 of 85 operations document a 404; all 85 document at least one non-2xx. Every
path-parameterised GET/DELETE therefore synthesizes an example asserting **404 at the
deliberately-absent name** — reachable, and live-checked PASS against the rehearsal service. The
one path-parameterised GET documenting no 404 was refused with the reason (the finding-11 fix's
refusal half), and `--observe-arg` bindings made two operations' documented successes reachable
(the constructive half). Both halves fired correctly with no operator intervention beyond the
bindings.

**Concluded.** gcp finding 11 generalised completely *within* Discovery-derived corpora (0 of
1,164 operations document any non-2xx); this measurement bounds it from the other side: it is a
property of the description format, not of API descriptions as such. Smithy models errors as
first-class shapes with `httpError` codes, and everything downstream of that single format fact —
satisfiable examples, a meaningful live-gate exit code, value-bearing corpora (finding 6) — holds
or fails with it. Where a worked example may come from is decided by the format a vendor chose.

## 3. A bodied operation's synthesized worked example violates the description's own schema

**Measured.** The adapter synthesizes `"{}"` as the body argument for all 33 requestBody
operations and asserts the documented success. **16 of the 33 declare `required` fields**, so for
those sixteen the example contradicts the description it was generated from — `CreateFunction`
requires `Code`, `FunctionName` and `Role`, and its example posts `{}` asserting 201.
`certify=OK` in every case, since certify checks the record, not the description. Live, the
rehearsal service answered the synthesized creates with 400/500 against documented 201/202/204
(6 of the 41 live-gate failures are this shape).

**Hermetic reproduction** — no vendor, no converter, no service, no credentials
([`repro/finding-3-required-body.openapi.json`](repro/finding-3-required-body.openapi.json), one
POST whose body requires `name` and `kind`):

```bash
python3 tooling/nl-ingest-openapi/openapi_ingest.py \
    evolution/aws-sdk-poc/repro/finding-3-required-body.openapi.json --out /tmp/f3
# createwidget   body=expr_a75b7d76b45c150…  certify=OK
python3 -c "import json; r=json.load(open('/tmp/f3/createwidget.v0.2.json')); \
  print([a['value'] for a in r['examples'][0]['args']], '->', r['examples'][0]['result']['value'])"
# ['http://localhost', '{}'] -> 201
```

**Concluded.** This is finding 11's family — *use a value nothing satisfies* colliding with
*assert the documented success* — one verb class over: the `25fe525` fix covered
path-parameterised GET/DELETE and left bodied operations carrying the same disease through a
different limb. The description states the unsatisfiability in its own words (`required`), so the
refusal needs no execution to justify. The suggested resolution mirrors the accepted one: an
operation whose required body fields nothing supplies has no spec-derivable worked example, and
refusing with the reason is more honest than asserting a success that cannot hold. The
constructive half is harder than `--observe-arg`'s, because the observation gate is read-only by
rule and these verbs mutate — supplying an operator body would make the example *satisfiable* but
could not make it *observed*, which is gcp finding 4 still standing, correctly. Asserting the
documented 400 at `{}` instead would be the vacuous-example trap of gcp finding 8 in new clothes.

**Resolved** in `2099326`, by the suggested resolution: an operation whose every declared request
media type requires fields the synthesized example cannot supply refuses with the reason — the
repro above now answers `CreateWidget SKIPPED: request body requires fields (`name`, `kind`) …`
and writes nothing (the transcript above records the defect as measured at `5aaffda`). A
description also offering a media type that admits the empty body keeps compiling. Adapter tests
63 → 66.

## 4. The corpus doubles as an emulator conformance probe

**Measured.** The 41 live-gate failures, by shape:

| shape | count | reading |
|---|---:|---|
| got 404, documented 2xx | 31 | absent names on success-only verbs, indistinguishable from routes the emulator does not implement (Lambda's 2026 API families) without per-route probes — not split |
| got 400/500, documented 2xx/404 | 6 | the synthesized `{}` bodies of finding 3 |
| **got 2xx, documented 404/202** | **4** | **the emulator is wrong**: moto answers 200 where AWS documents 404 (`DeleteEventSourceMapping`, `ListVersionsByFunction`, `ListLayerVersions` at absent names) and 200 where AWS documents 202 (`UpdateEventSourceMapping`) |

The four in the last row were caught by description-derived examples with no test authored by
anyone.

**Concluded.** An emulator-backed exit gate's verdict is about the emulator exactly as much as
about the records; the interesting direction is that the gate is *symmetric* — a corpus whose
examples assert documented outcomes is, run against an emulator, a conformance suite for the
emulator, for free. Rehearsal remains the right first tier (mutating verbs against a real cloud
during ingestion was gcp finding 4's correctly-rejected side effect), but a rehearsal PASS
licenses less than it appears to, and the real-service confirmation is not optional.
One provenance note: LocalStack's 2026 images refuse to start without a license token, which is
why the pinned rehearsal service is moto.

## 5. AWS moves auth AND endpoint from data to computation; the operator resupplies both

**Measured.** The Smithy model's `aws.auth#sigv4` converts to an `apiKey`-in-header scheme
(`x-amazon-apigateway-authtype: awsSigv4`) — but SigV4 is a signature computed per request over
method, URI, headers, body hash and date, so no static value substituted into an `Authorization`
header can satisfy it: a record generated from that scheme would certify and be unexecutable
against the real service with any credential the `{{secret:…}}` mechanism can carry. The
converted description also declares **no `servers`**: an AWS endpoint is an endpoint-rules
computation (region, FIPS, dualstack — an embedded decision diagram), not a constant. Left as
generated, every worked example synthesizes against the adapter's `http://localhost` default and
cannot execute anywhere.

**Concluded, and applied as the module's two description transforms.** Both facts are the same
fact: this vendor publishes *procedures* where the adapter's contract expects *data*, for
exactly the two fields that bind a record to an environment. The honest split is the one gcp
finding 7 chose for its bearer token, taken further: strip the unsatisfiable auth claim, set the
server to the operator's signing entry point, and let a local SigV4 proxy — operator machinery,
out of band, like `gcloud auth print-access-token` was — hold the identity. Record-side the
result is *stronger* than gcp's: there is no secret placeholder at all; credentials appear in no
record, no trace, and no argument; captured traces replay grantless, offline, identityless
(verified on the loop's create trace). What is given up is also explicit: the record no longer
states that the wire request is authenticated — that fact now lives in the operator's topology,
and a consumer replaying the trace inherits it as testimony about the observation, which is where
the trust model already prices it.

## 6. The corpus carries values — observed projections at scale

**Measured.** The live-gated run materialised **63 observed field projections** from two
`--observe-arg` bindings (`GetFunction`, `GetFunctionConfiguration` at an operator-created
function) plus the constructible list operations — typed fields like `Runtime`, `Role`, `State`,
`FunctionArn` as `Maybe string`, structured ones as `Maybe Json`, each with a trace-attached
observed example. Three declared-`number` fields (`CodeSize`, `MemorySize`, `Timeout`) were
honestly *not* projected, with the reason (JNum carries int or float; a typed numeric promise
cannot be narrowed soundly by pattern alone).

**Concluded.** gcp finding 3's binding constraint — a corpus of operations returning `int`,
nodes without edges — was a fact about `*/*` responses and undocumented errors, not about
API-derived corpora. With JSON media types and documented outcomes, spec plus observation license
value-bearing records at scale, and the material for assembly-by-replay (gcp finding 10's
resolution) actually exists here. Whether multi-stage composition over such records is worth
attempting remains the open question that module recorded; this corpus is the first one derived
from a real cloud API on which it could even be tried.

## 7. The live service serializes absent members as explicit `null` — and the description does not admit it

Found during the live confirmation's slice ingestion (2026-08-20), by the observation gate
itself.

**Measured.** `ListFunctions` on an empty account answers `{"Functions":[],"NextMarker":null}`.
The Smithy-projected schema declares `NextMarker` an *optional string* — present-as-`null` is
not a value that schema admits, so the whole-document projection `ListFunctionsBody` was
REFUSED by the observation gate (`property NextMarker is not the declared string`) while the
field projection legitimately observed `None` and `Functions` observed `Just []`. The pattern
generalises: `GetFunction` and `GetFunctionConfiguration` documents at a real function carry
~20 explicit-`null` members (`VpcConfig`, `Layers`, `KMSKeyArn`, …), so **both their
whole-document projections refused while all 43 field projections materialised**;
`GetAccountSettingsBody` materialised — its document happens to carry no `null`s.

**Concluded.** The real service (restJson1 as Lambda serves it) emits explicit `null` for
absent structure members; the Smithy→OpenAPI projection encodes absence as *omission*. Between
those two conventions sits every whole-document projection: held to the declared shape, it
refuses on the first nulled member, exactly as the finding-8 discipline demands — and the
per-field verdicts stay honest (`None` for a nulled member is a true observation of the
document obtained). This is finding 2's lesson from the other side: what a worked example may
assert is decided by format facts, and here two format layers of the *same vendor chain*
disagree about what absence looks like. A resolution would have to decide which layer speaks
for the description — admit `null` for optional members at conformance time, or keep the
refusal as the honest reading of the schema's own words. Recorded as a measurement; the
refusal is not obviously wrong.

**Resolved** (maintainer decision, applied same day): **admit** — present-as-`null` on a
declared *optional* member is this wire format's spelling of absence, and the document
conforms. The decisive argument was consistency, not leniency: the field projections already
read `null → None` as a legitimate observation of an obtained document, so refusing the same
fact at document grain was an inconsistency between grains — and the `+json` precedent
applies (the serialization fact *is* the promise). The same reading sharpens the other edge:
a **required** member spelled `null` is spelled *absent*, and a required member may not be
absent — that now refuses, where previously an untyped required member could carry `null`
silently. Only `null` is absence; a present non-`null` value of the wrong type stays a
violation. Adapter tests 71 → 75.

## Reproducing

Finding 3 is hermetic; the commands are inline above and need only this repository.

Findings 2, 4 and 6 need the generated corpus and the rehearsal service. The pipeline lives
outside this repository; every input is pinned in the provenance table. In outline:

```bash
# Smithy model (pinned commit) -> OpenAPI 3
smithy build          # openapi plugin, service com.amazonaws.lambda#AWSGirApiService
# normalize: strip sigv4, set servers to the signing entry point (finding 5)
# moto_server on :4566; a SigV4-signing proxy on :9099 forwarding to it
python3 tooling/nl-ingest-openapi/openapi_ingest.py lambda.openapi3.normalized.json \
    --out records/ --verify-against http://127.0.0.1:9099 \
    --observe-arg GetFunction.FunctionName=<operator-created-function> \
    --observe-arg GetFunctionConfiguration.FunctionName=<operator-created-function>
# the loop: four apply-expressions over the records' fn_ refs, run by
#   nl-validator eval --records records/ --grant net.{read,write}@127.0.0.1 --trace-out …
```

Finding 1 needs only the repository at `5aaffda`:
`manage.py test commons` fails 3; at `e27b281` it passes 173.

The live provisioning step against real AWS needs an account and is the module's named next
step; no finding above depends on it.
