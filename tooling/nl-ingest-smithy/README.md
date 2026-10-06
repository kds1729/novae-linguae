# nl-ingest-smithy — Smithy models (AWS JSON-RPC) → Nova Lingua records

`smithy_ingest.py` reads an AWS service **as AWS publishes it** — a [Smithy](https://smithy.io)
model in JSON AST form, one file per service in `aws/api-models-aws` — and emits one **verified**
Nova Lingua record per operation of a service whose protocol is `awsJson1_0` or `awsJson1_1`
(plus, for a read observed at a success, one typed projection per top-level output member). It is
the third description-layer adapter after [`nl-ingest-openapi`](../nl-ingest-openapi/) and
[`nl-ingest-graphql`](../nl-ingest-graphql/) and shares their doctrine: the description *is* the
semantic content of a client call; every record is gated through `nl-validator certify`; every
worked example is a recorded observation that replays offline.

Why a third adapter: the `aws-sdk-poc` route (Smithy → `smithy-cli` OpenAPI projection →
`nl-ingest-openapi`) exists only for `restJson1` services such as Lambda and EKS. Most of AWS
speaks **awsJson** instead — ECS, ECR, Cloud Map, Global Accelerator, Cloud Control, … — and that
protocol does not project to OpenAPI at all (the converter fails on it). Its wire shape is one
endpoint, every operation a `POST`, the operation named by the `X-Amz-Target` header
(`<ServiceShapeName>.<Operation>`), a JSON input, a JSON output. Three consequences shape the
adapter:

1. **The input is caller data.** A Smithy input structure nests freely and the members a real
   call needs are mostly *optional* — an `ECS CreateService` that provisions anything is never the
   "minimal documented call". So every record takes the whole input as **one `Json` parameter**,
   `(base, input) → …`, serialized by the existing `render_json` builtin (the GraphQL variables
   precedent: no string splicing, no new builtin). The model's `required` members are read, but as
   a **contract the service enforces** (below), not as the record's parameter list.
2. **The effect is `net.write`, for every operation.** The validator classes an `http` call by
   method and every awsJson call is a `POST`. The model's `smithy.api#readonly` trait *could*
   refine that, but it is not reliably present — measured on the five models this adapter was built
   for, ECS marks 29 of 77 operations and Global Accelerator, Cloud Control, ECR and Cloud Map mark
   **none**. A trait the description may omit cannot license a weaker effect; the method rule stands
   and the records are honest about the wire. `readonly` — by trait, or declared by the operator
   with `--readonly <glob>` — licenses only what it can: *observing* the operation during
   ingestion, and the `query/lookup` intent tag.
3. **Nothing is spec-derivable.** awsJson success is always `200`, and the model does not describe
   the validation error. So a record exists **only through the observation gate**
   (`--verify-against`); without it the adapter prints a licensing report and writes nothing.
   Which call an operation is observed *at* is the model's decision — see the table.

## The decision table

| the model says | what the gate observes | record(s) |
|---|---|---|
| readonly, no required member | the empty input `{}` is a **valid** call → expect `200` | `<Op> : (string, Json) → int` + the output projections |
| **any** operation with a required member | `{}` **violates** the model's own `required` contract, so the service must reject it *before acting* → expect a non-2xx carrying an awsJson error `__type`; the record's example is the status the service answered | `<Op> : (string, Json) → int` |
| readonly with required members, `--observe-arg <Op>.input=<json>` | the operator's input (real server state the model cannot name) → expect `200` | `<Op>` + the output projections |
| mutating, no required member | the empty input is a valid call, so **no effect-free observation exists** → **refused** | — |
| mutating, `--observe-effect <Op>.input=<json>` | the explicit opt-in: the gate **performs the effect once** at the operator's input → expect `200` | `<Op>` + the output projections (the request token, the ARN — what a plan threads onward), all from that ONE effect's trace; the examples carry the operator's values |

The second row is the adapter's constructive answer to the question the OpenAPI adapter left
open after `aws-sdk-poc` finding 3 (creates with required bodies refuse): the **effect-free call of
a mutating verb is its rejected call**, and a required member makes `{}` rejected *by the
description's own promise*. `CreateService {}` costs nothing, changes nothing, and proves the record
speaks the protocol (target header, content type, JSON body, error envelope). It is the gcp
proposal-02 move — `DELETE` at the absent name — carried to creates. If the service answers 2xx to
`{}` anyway, the description lied: the gate **fails loudly**, and for a mutating verb says an
effect may have occurred.

## Mapping

| Smithy (JSON AST) | Nova Lingua |
|---|---|
| operation `Op` (bound to the service directly or through its resources' lifecycle, `operations`, `collectionOperations`, child resources) | the **leaf** record `Op : base, input → int` — the response status |
| `aws.protocols#awsJson1_0` / `awsJson1_1` on the service | `Content-Type: application/x-amz-json-1.0` / `-1.1` (a spec-time literal) |
| the service shape's name | `X-Amz-Target: <ServiceShapeName>.<Op>` (a spec-time literal) |
| the input structure | one `Json` parameter, `render_json input` as the body; `required` members = the enforced contract (table above) |
| the output structure (observed at `200` — a read, or an effect under the opt-in) | `OpOutput : … → Maybe Json` (the whole document) + per top-level member: string / enum / blob → `Maybe string`, boolean → `Maybe bool`, structure / list / set / map / union / document → `Maybe Json`; numeric and timestamp members **noted, never projected** (`JNum` carries int or float; awsJson timestamps are epoch-seconds numbers) |
| `aws.api#service.sdkId` | the short form in hints and tags (`ecs`, `global-accelerator`; the shape name when absent) |
| the endpoint | the `base` parameter: for AWS the operator's **SigV4-signing entry point**, which also fixes service and region — the same record provisions in every region |
| `aws.auth#sigv4` | nothing in the record (`aws-sdk-poc` finding 5): a computed signature is the operator's boundary; traces replay identity-free |

Files are named by the operation lowercased (`ListClusters` → `listclusters.v0.2.json`,
`body-listclusters.json`, `trace-listclusters-0.json`); the run report prints the file next to
each record. One service per `--out` directory (operation names repeat across services —
`ListTagsForResource` is in all five). Each record's `intent_tags` are `io`, `io/network/http`,
`query/lookup` when readonly, `parse` for a projection, plus one extending tag carrying the
service's short form (`query/lookup/ecs-list-clusters`, `io/network/http/ecs-create-service`,
`parse/ecs-list-clusters-output`; omitted, never truncated, past 64 characters).

## The observation gate

With `--verify-against <endpoint>` each planned operation runs **once** through its leaf record
(`nl-validator eval --trace-out` under `net.write@<host>`); the trace's real status and body are
judged against the table; the leaf is minted with the observation as its trace-attached worked
example. On a `200` the output document is held to the declared structure — every `required`
member present and non-null, every present member of its declared type (enum values in the
declared set, structures and lists recursively), an optional member's explicit `null` reading as
absent (the finding-7 decision) — and only then do the projections mint, each by `eval --replay`
of the leaf's trace: **one request per operation** however many records it licenses (the same
observation, the same `trc_` address; `--pace SECONDS` spaces the live calls). Large observed
values ride by address above `--blob-threshold` JCS bytes (default 64 KiB). After the gate every
record is certified and **replayed with no service** (`nl-validator run`) — the offline check any
commons consumer can perform.

## Honest refusals

A service speaking anything but awsJson (restJson1 is told its route; the Query/XML protocols have
none); a mutating operation with no required member and no `--observe-effect` opt-in; an
`--observe-arg` on a mutating operation (an observation must not create state — the opt-in is a
different flag on purpose); an `--observe-effect` on a readonly one; a bound input lacking a
required member (the service would reject it); a binding naming an operation the service does not
declare — refused **before any artifact is written or any call is made**. At the gate: a 2xx to
`{}` where the model requires members (the description does not hold); a non-2xx without an error
document (a transport refusal is not the protocol's rejection); an error document whose `__type`
refuses the *request* rather than the input — `AccessDeniedException`, signature and token errors,
throttling, an unknown target, a server fault (measured live: an out-of-boundary AWS call answers
`400 AccessDeniedException`, the very status a validation rejection carries, with the input never
looked at); a non-200 where 200 was expected;
a document violating its declared shape — each fails that operation with the reason, mints
nothing, and the run exits 1.

```
python3 smithy_ingest.py examples/item-rpc.smithy.json --out /tmp/recs \
    --verify-against http://127.0.0.1:8878/rpc \
    --observe-arg 'GetItem.input={"name":"rpc-widget"}'
# AWS: python3 smithy_ingest.py ecs-2014-11-13.json --out recs/ecs \
#          --verify-against http://127.0.0.1:9099/ecs/us-east-1 --readonly 'List*' --readonly 'Describe*'
```

[`examples/item-rpc.smithy.json`](examples/item-rpc.smithy.json) is the model of the in-repo
[fake service](../fake-service/)'s `/rpc`: four operations covering every row of the decision
table (`ListItems` readonly/no-required, `GetItem` readonly/required with a 404 error shape,
`PutItem` mutating/required, `ResetAll` mutating/no-required). `tests/` gates against it and against
two lying services: 27 tests, `python3 -m unittest discover -s tests`.

## Measured on the real models (offline licensing, 2026-10-06)

`aws/api-models-aws` at `7eb6ab98`: ECS 72 of 77 operations plan (106 records licensed; refused:
`CreateCluster` and four agent-internal operations, all mutating with no required member);
Global Accelerator 51/56, Cloud Control 7/8, ECR 46/58, Cloud Map 27/30 — every refusal an
unmarked read (`List*`/`Describe*`/`Get*`) that `--readonly` recovers, or a mutating operation
whose empty input is valid.

**Live against real AWS (same day, through a local SigV4 signing proxy, an otherwise empty
account): 258 records certified and offline-replayed from 221 calls, nothing created.** ECS 93
(from 72 calls), ECR 43, Cloud Map 37, Global Accelerator 70/70, Cloud Control 15/15 (including a
real `ListResources AWS::ECS::Cluster` observation via `--observe-arg`). Every rejection row held:
no service accepted `{}` where the model requires a member. What the gate refused, each a
description-level finding: **authorization order varies per operation** — ECS validates
`CreateService {}` before authorizing (the rejection costs no write permission) but authorizes
`DeleteCluster` first (`AccessDeniedException`, the same 400 as a rejection — hence the
request-refusal rule above); **"optional" in the model, required by the world** — `ListServices`/
`ListTasks`/`ListContainerInstances` at `{}` answer `ClusterNotFoundException` because the implied
default cluster does not exist; **a hidden server fault** — `ListServicesByNamespace {}` answers
`500` where the model calls `namespace` optional; **a host prefix the record cannot carry** —
Cloud Map's `DiscoverInstances*` live on `data-servicediscovery…` (`smithy.api#endpoint`), noted at
plan time, the operator's `base` to honor; and ECR's registry-level operations denied by a
deliberately repository-scoped permission ceiling. Observed outputs carry account identifiers
(ARNs; `GetAuthorizationToken` even a short-lived login token), so such records are the
operator's to publish or not — the `aws-sdk-poc` ARN boundary.

Reuses [`ingest-common`](../ingest-common/). Requires only `python3` and the built `nl-validator`
(sibling build, the quickstart's fetched binary, or `NL_VALIDATOR`).
