# Findings — Google Cloud Storage v1 through `nl-ingest-openapi`

Companion to [`README.md`](README.md). Per [`evolution/README.md`](../README.md), measurements and
inferences are kept separate: each finding states what was observed, then what the author concludes
from it. A reader may reject every conclusion here and keep every number.

## Provenance

| input | pinned value |
|---|---|
| Discovery document | `storage:v1`, revision **20260719**, from `https://storage.googleapis.com/$discovery/rest?version=v1` |
| Discovery → Swagger 2.0 | `google-discovery-to-swagger` **2.1.0** (npm) |
| Swagger 2.0 → OpenAPI 3 | `swagger2openapi` **7.0.8** (npm) |
| upstream commit | `76fc6ba` (2026-07-15) |
| run date | 2026-07-26 |

Google publishes Discovery documents rather than OpenAPI 3, so the pipeline is
`Discovery → Swagger 2.0 → OpenAPI 3 → normalize → nl-ingest-openapi → commons node`.

## What was measured

| quantity | value |
|---|---|
| operations in the converted description | 81 |
| operations compiled | 81 |
| operations refused or skipped | 0 |
| records that certified | 81 |
| operations with a `requestBody` | 31 (30 `application/json`, 1 `application/octet-stream`) |
| 2xx responses declaring a body | 69 |
| distinct 2xx response content types across the spec | 1 — `*/*` |
| body projections actually licensed by the run | **0** — `*/*` fails the media-type check first |
| operations that would pass the constructibility filter, computed from the spec | 1 (`storage.buckets.list`) |
| notes emitted by the run | 877, all "optional query param … omitted" |
| notes about the 69 declined response bodies | 0 |
| records with non-empty `refinements` | 0 |
| modifications to this repository required | 0 |

Live provisioning: bucket `insert` → 200, `get` → 200, `delete` → 204, each asserted by the
executed record's example, effects granted from `signature.effects`, bearer token supplied as
`--secret`.

---

## 1. The request `Content-Type` is dropped

**Measured.** A hand-authored OpenAPI 3 description with one `POST` whose `requestBody` declares
`application/json` compiles to a body AST whose `http` header argument is
`{"kind": "var", "name": "map_empty"}`. Neither generated artifact contains the string
`content-type`. The run reports `certify=OK`. 31 of the 81 Cloud Storage operations are affected —
every write operation in the API. No converter is involved in the reproduction.

Multipart bodies are unaffected: they have always emitted a `Content-Type` carrying the spec-time
boundary. Only the non-multipart path is missing it.

**Concluded.** The record cannot perform its own documented call, and nothing in the run report says
so, which makes this a faithfulness defect rather than a missing feature. The repair must be
data-driven from each operation's own declared media type — a blanket `application/json` would be
wrong for `storage.objects.insert`, which declares `application/octet-stream`. See
[proposal 01](proposals/01-request-content-type.md).

**Resolved.** [Proposal 01](proposals/01-request-content-type.md) accepted and applied: the adapter
emits the single declared non-multipart media type, and notes-and-omits when a description declares
more than one. The re-addressing it causes was taken deliberately rather than deferred to a version
boundary — the affected records could not execute, and the real defect was the false `certify=OK`.

**Residual, recorded rather than acted on.** The note-and-omit branch leaves the multi-declared-type
case in the state decision 1 identified as the actual defect: body present, no `Content-Type`,
`certify=OK`. The symmetry with finding 2 that justifies it is not quite exact — declining a
*projection* still leaves a working status record, while declining a *request* header leaves one a
strict service answers 415 — and a generation-time note does not travel with the record to a commons,
which is the same "diagnostics stay local, records travel" gap that made finding 2 invisible at
scale. **Zero of the 81 operations here declare more than one request media type**, so this is
latent, not live, and it is logged instead of reopened. If a real description hits it, the principled
resolution is probably to make the media type a caller parameter — the description says "one of
these", which makes the choice caller data, the same literal-scaffold/caller-data split `url_encode`
established — rather than to refuse the operation outright.

## 2. A non-JSON 2xx response body was declined silently

**Measured.** Two descriptions differing in exactly one key — the response content type — produce
different amounts of explanation. With `application/json`, four notes naming the pending projections
(`getStatusBody`, `getStatusState`, `getStatusHealthy`) and why each was not materialised. With
`*/*`, the entire output is:

```
getstatus        body=expr_09e7efcc93b0153…  certify=OK
```

The body hash is identical in both runs, so only the projections diverge. At scale: 877 notes
emitted, none about the 69 declined response bodies.

The trigger is not vendor-specific. `google-discovery-to-swagger` emits Swagger 2.0 with **no
`produces`** — not global, not per-operation — while still declaring response schemas.
`swagger2openapi` then has no media type to key the response content by. Confirmed on a hand-written
four-line Swagger 2.0 document with nothing Google about it:

```
no `produces`                    -> content keys: ["*/*"]
`produces: [application/json]`   -> content keys: ["application/json"]
```

**Concluded.** Declining to project on `*/*` is *correct* — a media range promises any type, so it
cannot license the parses-as-JSON promise `parse_json` needs. Declining **silently** is the defect,
because it contradicts the adapter's own contract that what it cannot carry gets a printed reason.
`produces` is optional in Swagger 2.0 and routinely omitted, so any description arriving through
that conversion path loses every body projection without a word — a large class, and Swagger 2.0
conversion is a common way OpenAPI 3 descriptions come into existence.

**Resolved.** Fixed in #1 (`2659c09`): the adapter now names the media type it declined and why.
Only the note changed — whether `*/*` accompanied by a JSON Schema should be *treated* as JSON is a
separate judgement, deliberately not taken. Diagnostics only, so no generated artifact moved.

One detail from fixing it, worth keeping: the existing suite had locked the silence in.
`test_suffixed_json_content_type_licenses_schema` asserted `text/html` → `pending == []` and never
checked whether anything was said, so the assertion was extended rather than replaced.

## 3. The constructibility rule would admit 1 of 81 operations

**Measured — in the run: zero.** No body projection was licensed at all, because every 2xx response
arrives as `*/*` (finding 2) and fails the media-type check *before* the constructibility rule is
reached. Nothing in the shipped corpus is a projection.

**Computed from the spec, not observed.** Applying the constructibility filter — a bodyless `GET`
with no path parameters — to the description directly, with a script rather than through the adapter:

| filter | operations |
|---|---|
| total | 81 |
| 2xx response declaring a body | 69 |
| `GET`, no path parameters, no request body | **1** (`storage.buckets.list`) |

So 1 is what the rule *would* admit once the media type no longer gates it. Everything under
`/b/{bucket}` — `buckets.get`, `objects.get` — is excluded by the path parameter, and every mutating
verb by the verb.

**Concluded.** The rule's reasoning is sound: a path parameter names server state the description
cannot promise, so no worked example is derivable from the spec alone. But the consequence is not
visible from the in-repo reference descriptions, and it is severe. Every generated leaf record
projects `.status` and discards `.body`, so the corpus is a set of operations returning `int`. Values
cannot flow between calls, which means the corpus **cannot express a dataflow plan** — nodes without
edges — no matter how many operations are ingested.

This is a constraint on where a worked example may come from, not on runtime capability: `http`
already returns `{status, body}`, and a record that parsed `.body` would execute correctly. It simply
could not be certify-gated the same way. The live gate already sources examples from observations
elsewhere, which is why open question 1 in the README is phrased as it is.

## 4. The live observation gate is unusable for mutating verbs

**Measured.** Schema-derived projections materialise only under `--verify-against`, which executes
each example once. The pipeline withheld the flag entirely, so no schema-derived projection could
materialise for this corpus.

**Concluded.** For `storage.buckets.insert` the gate would create a real bucket during *ingestion*,
which is not an acceptable side effect of building a corpus. `GET`/`HEAD` are `net.read` and create
nothing, so a read-only gate would be safe and would materialise projections wherever finding 3
permits them. Whether that belongs behind a separate flag is a maintainer's call.

## 5. What the description layer cannot supply

**Measured.** All 81 records carry `refinements: []`, `derived_from: null`, and `supersedes: null`.

**Concluded.** An OpenAPI description carries no pre/postconditions and no lineage. Google's document
says a subnet takes a `network` field; it never says a network must exist first, because that is not
a fact about the API surface — it is a fact about the domain. **Dependency and ordering knowledge is
not manufacturable from descriptions at any scale.** A corpus grown this way is a vocabulary, and the
knowledge of how to assemble it has to be authored, as composite records or refinements or both.

This is scoping, not criticism: the adapter does exactly what it claims. It does mean that the value
of a commons for assembling systems rests on a layer this pipeline cannot produce.

## 6. Resolution needed to be exact, not semantic

**Measured.** Operations were resolved for execution with `POST /v0/query {name_hint_prefix}`.
`/v0/search` was tried first and was not reliable for selecting one operation out of 81 sharing four
near-identical intent tags.

**Concluded.** The node's stdlib embedder is lexical — it ranks by token overlap, not meaning — and
the extending `<lead>/<own-hyphenated-name>` tag helps but does not make ranking dependable at this
density. Exact prefix query is the right tool when the caller knows the operation's name, which is
the case for execution. It is unavailable to a caller searching for a *design* it cannot name, which
is the case for finding prior art.

## 7. The description transforms — one applied, one identified and not applied

**OAuth2 → a plain `Bearer` HTTP scheme.** GCP Discovery declares OAuth2 (implicit /
clientCredentials), and `resolve_auth` refuses interactive flows because a browser/redirect principal
cannot cross the effect boundary. That refusal is correct. But every GCP call is, on the wire,
`Authorization: Bearer <token>`, and `gcloud auth print-access-token` yields such a token out of
band. Rewriting the scheme leaves the wire request byte-identical and changes only the *description*
of how the token is obtained.

**Concluded — deliberately not proposed upstream.** As a general transform it would let any
description launder past a boundary this project chose on principle. If it were ever wanted, the
honest shape is narrower: an explicit operator-supplied bearer credential for an oauth2-declared
operation, recorded as operator-supplied.

**`*/*` → `application/json` on responses — identified, NOT applied.** An earlier revision of this
document described this as a second transform the pipeline performs. It does not: the pipeline
applies only the security normalization above, and the ingested description still carries `*/*` on
all 69 body-bearing responses. That is precisely why the run licensed zero projections (finding 3).

The transform remains the right downstream compensation for the conversion artifact in finding 2 —
Cloud Storage is independently known to serve JSON, so declaring what it actually serves is faithful
rather than a workaround — and applying it is the necessary first step to licensing any projection
from this description. It is still not proposed upstream: the adapter's correct fix is the note, and
a description that means `application/json` should say so.

## 8. An optional field projection materialises off a FAILED call

Found after the module was first published, while applying `*/*` → `application/json` (finding 7) and
then pointing the observation gate at real GCS. Reproduced against upstream `c482645`.

**Measured — hermetically, no cloud and no credentials.** A bodyless `GET` with no path parameters
(so its projections are constructible), a literal path the in-repo fake service does not serve, and a
declared 200 schema whose two properties are **optional**. The call answers 401; the gate then
disagrees with itself:

```
getAbsentBody    observation-gate=FAIL: live response did not yield the declared 200 JSON document
getabsent        live-gate=FAIL: example 0 live result does not match the documented one: 401 != 200
getabsentalpha   live=OBSERVED+schema-checked  certify=OK  examples=PASS (replayed offline)
getabsentbeta    live=OBSERVED+schema-checked  certify=OK  examples=PASS (replayed offline)
```

Two records are written, each with `result: {"kind": "variant", "tag": "None"}` and a trace of the
failed request. The same behaviour reproduces against live GCS, where a synthesized `?project=hello
world` answers 400 and four `storage.buckets.list` field projections materialise identically.

**The branch is exact** (`openapi_ingest.py`, `materialize_schema_projection`):

```python
is_none = … got.get("tag") == "None"
if p["field"] is None:                    # whole document
    if is_none:
        return False, "live response did not yield the declared … JSON document …"
    …_value_conforms(…)
elif p["required_field"] and is_none:     # a REQUIRED field
    return False, "required property … absent or mistyped …"
# an OPTIONAL field with is_none falls through and materialises
```

**And the guard that would catch it cannot fire on a Discovery-derived corpus.** Google's Discovery
documents emit no `required` at all: **0 of the 34** schemas in the Cloud Storage description declare
one, so `required_field` is `False` for every field projection in the corpus. The `elif` is dead code
for this whole class of description, and every field projection of a failed call materialises.

**Concluded.** For an optional field, `None` is a legitimate observation when the document *was*
obtained and the field was simply absent. The gate never distinguishes that from `None` because no
document was obtained at all — and the information needed to tell them apart is already computed, two
branches up, by the whole-document projection.

Three consequences, in increasing order:

1. **The `schema-checked` label is false.** Nothing was held to the declared shape, because there was
   no document to hold.
2. **The record is vacuous but certified.** A `Maybe`-typed projection whose only worked example is
   `None` demonstrates nothing about extracting the field; `None` is what it returns for any failing
   call. It ships `certify=OK`, `examples=PASS`, trace attached, ready to publish.
3. **The gate contradicts itself** — one run, one response, opposite verdicts. That needs no
   agreement about what *should* happen; the adapter already disagrees with itself.

The failure mode is the one proposal 01's decision 1 named: an artifact reporting itself verified
when its evidence establishes nothing. And an expired token would silently mint a `None`-valued,
`certify=OK` projection for every field of every operation in a corpus.

**Smallest fix that restores consistency:** let the field projections inherit the whole-document
verdict. It already computes exactly the right conclusion for the whole response; that conclusion
applies to every projection over it. Failing that, at minimum do not print `schema-checked` when no
document was checked.

---

*Findings 9, 10 and 11 come from pointing the architect machinery — `check-plan`, `assemble` — at this
corpus for the first time, at upstream `c482645`. Nine and ten are not defect reports: the plan checker and
the assembler each behave exactly as specified, and both are about what a corpus derived from an
API description can and cannot feed them. Eleven is a defect, and the live gate is what exposed it.*

## 9. World refinements cannot key a resource the request body names

**Measured — the corpus as shipped approves a use-after-delete.** A two-step plan, delete then read
the same bucket, checked against the 81 records:

```
PLAN-SOUND   every requirement discharged from assumptions and prior ensures
exit=0
```

Every record carries `refinements: []` (finding 5), so there is nothing to contradict. The verdict is
honest — vacuously.

**Measured — with refinements authored, the checker works.** `storage_buckets_get(base, bucket)` and
`storage_buckets_delete(base, bucket)` take the bucket as a parameter, so both are expressible:

```json
{ "kind": "requires", "resource": { "class": "bucket",
    "key": [ { "kind": "var", "name": "bucket" } ] }, "state": "exists" }
{ "kind": "ensures",  "resource": { "class": "bucket",
    "key": [ { "kind": "var", "name": "bucket" } ] }, "state": "absent" }
```

Three plans, three verdicts:

| plan | verdict |
|---|---|
| delete → get | `REJECTED` — *step 2: requires bucket("nl-plan-demo") = exists, but it is absent (step 1 (storage_buckets_delete))* |
| create → verify → delete | `UNVERIFIABLE` — 2 requirements nothing establishes |
| the same, plus a stated assumption | `PLAN-SOUND` |

**Concluded — the correct plan is the one that cannot verify.**
`storage_buckets_insert(base, project, body)` carries the new bucket's name **inside the JSON body
argument**. A resource key may reference parameters by name or literals, and the name is neither, so
`ensures bucket(…) exists` is unexpressible — nothing discharges the later `requires`. The only route
to `PLAN-SOUND` is the third plan's assumption, and that assumption is *false at plan start*: it
asserts the bucket exists before the step that creates it.

**This is the REST creation idiom, not a GCS quirk.** All **9 of 9** create operations name the new
resource in the request body; their path parameters name the *container*:

| operation | path params | where the new resource is named |
|---|---|---|
| `storage.buckets.insert` | — | body |
| `storage.folders.insert`, `…managedFolders…`, `…notifications…`, `…anywhereCaches…`, `…bucketAccessControls…`, `…defaultObjectAccessControls…` | `bucket` (the container) | body |
| `storage.objectAccessControls.insert` | `bucket`, `object` (both containers) | body |
| `storage.objects.insert` | `bucket` (the container) | body — **or** `?name=`, which is *optional* and therefore dropped |

`objects.insert` is the sharpest case: the API does offer a URL-level name, and two rules interact to
remove it — the minimal-documented-call rule drops optional query parameters, so the one parameter
that could have keyed the resource never reaches the record.

`ensures` on creates is exactly the clause a lifecycle plan needs, so the gap falls on the half that
matters. Widening the key vocabulary to reach into a body argument would mean the checker parsing
caller data, which is a real design question rather than an oversight — noted here as the driver the
v0.1 spec says richer vocabulary should be earned by.

## 10. `assemble` cannot admit any record from this corpus

**Measured.** A goal one record satisfies exactly — given a base URL and a bucket name, produce
`Just("US")`, which is `storage_buckets_getlocation`'s own worked example — against a node holding
the corpus and its bodies:

```
discovered 120 candidate(s); arity-pruned 65 (unusable at arity 1..=2), fetching 55 by content-address…
NO PIPELINE  no composition of ≤2 commons functions reproduces the 1 example(s)
```

The cause is `record_solves_goal` in `assemble.rs`, and it is deliberate:

```rust
let inf = crate::infer_effects(body, records);
let safe = inf.effects.is_empty() && !inf.opaque && !inf.unresolved
        && matches!(crate::analyze_termination(body), TerminationOutcome::Always);
if !safe { return false; }
```

documented as *"discovered code is never executed on spec"*, with a test asserting *"an effectful
body is never executed on spec, whatever it might return."*

**Measured — the exclusion is total.** Of the 121 records (81 status + 40 observed projections),
**121 are effectful** — 70 `net.read`, 51 `net.write` — and **0 are pure**. The purity gate rejects
every candidate, so no assembly over an API-derived commons can ever succeed.

**Measured — the evidence assemble needs already exists, effect-free.** The same record replays its
worked example offline, with no grants, no secrets and no network, producing precisely the goal's
expected output:

```
$ nl-validator run storage_buckets_getlocation.v0.2.json --records .
example  0  PASS  {"kind":"variant","payload":{"kind":"string","value":"US"},"tag":"Just"}
run: 1/1 examples passed
```

**Concluded.** Two correct principles are in tension. Assembly verifies a candidate by *running* it
against the goal's examples; and discovered code from an untrusted commons must never be executed on
spec — a search choosing which network writes to perform would be reckless. Both are right, and
together they exclude precisely the corpus an infrastructure architect needs to compose, because
service operations are effectful by definition.

The resolution suggested by the measurement above: match an effectful candidate against a goal by
**replaying its recorded example** rather than executing it. That performs no effect, so it preserves
"never execute discovered effectful code" exactly as written; the evidence is the publisher's
trace-attached observation, priced by the trust model like any other observed claim; and the
machinery already exists and already works (GW12). Whether the faithfulness contract should accept
replayed evidence for goal-matching is a maintainer's question, not a patch.

**A defect in this PoC's pipeline, since fixed.** It published only *function records* — never the
body ASTs, and never the observation traces — so a node loaded from it held records that could not be
fetched for execution. The first `assemble --node` run skipped all 120 candidates as `absent`;
publishing the bodies dropped that to 4 (the `trc_…` artifacts the observed examples reference), and
publishing the traces too dropped it to **0**. A commons that cannot execute or replay its own corpus
is not a commons, and the replay-based resolution suggested above would have been impossible from a
node without the traces.

## 11. A path-parameterised GET's worked example is unsatisfiable by construction

Found by the first pass that ever live-gated this corpus.

**Measured.** `_example_for` fills a path parameter with `gw7-absent-x` — deliberately "a name no
test writes" — and then chooses the expected status:

```python
if verb in ("GET", "DELETE"):
    if path_param_names:
        want = 404 if 404 in codes else (codes[0] if codes else 200)
```

**Not one of the 81 operations documents a 404** — Google Discovery describes only the success case.
So `want` falls through to `200`, and the example asserts *a GET on a deliberately-absent bucket
returns 200*. Live:

```
storage_buckets_get   live-gate=FAIL: example 0 live result does not match the documented one:
                      {"kind": "int", "value": 404} != {"kind": "int", "value": 200}
storage_buckets_list  live-gate=FAIL: … {"kind": "int", "value": 400} != {"kind": "int", "value": 200}
```

`buckets.list` is the same shape one level over: its required `project` query parameter gets the
synthesized `"hello world"`, which cannot produce the documented 200 either.

**Concluded.** Two conventions that are individually sound contradict each other whenever a
description omits its error cases: *use a name nothing has written* and *assert the documented
status*. Against the in-repo fake service they agree, because it documents its 404s. Against a
Discovery-derived description they never can — the example is false for every operation of that
shape, and `certify` passes it because certify does not execute. It is the same family as findings 1
and 8: an artifact that reports itself verified while carrying a claim nothing has checked.

It also interacts with `--observe-arg`, which binds the *projections'* arguments but not the status
record's. In the run above, 44 projections observed and schema-checked cleanly while both status
records failed — so a live gate over such an operation reports failure even when every observation
succeeded, which makes the exit code useless as a signal.

**Suggested resolution, reusing what already exists:** when an `--observe-arg` binding names a real
resource for an operation, use it for the status record's example too. The operator has supplied a
value that *does* exist, so the documented success becomes reachable and the example becomes
checkable. Failing that, an operation whose description documents no non-2xx outcome has no
spec-derivable example for the absent-name convention, and saying so would be more honest than
asserting a success that cannot hold.

## 12. `certify` admits records the commons cannot accept

**Measured.** Ingesting `iam v1` produced 48 records that all certified, and the node then refused
seven of them:

```
$ manage.py loadrecords iam-v1.jsonl
reject schema_invalid: validation failed (1 error):
  - at /name_hints/1: "iam_locations_workforce_pools_providers_scim_ten…
stored=89 skipped=0 failed=7
```

`function-record.v0.2.schema.json` caps a `name_hint` at **64 characters**. Google's deeply-nested
resource paths sanitise well past that — IAM reaches **100** — and
`iam.projects.locations.workloadIdentityPools.namespaces.managedIdentities.…` is not an exotic
operation, it is how that API is shaped. `nl-validator certify` is untroubled:

```
$ nl-validator certify iam_locations_workforcepools_providers_scimtenants_tokens_create.v0.2.json …
  => CERTIFIED
```

**Concluded.** `certify` does not validate a record against the function-record JSON schema, so the
producer and the admitting authority disagree about what a valid record is. A record can be
generated, certified, written to disk and reported as verified, and still be unpublishable — which
is the same family as findings 1, 8 and 11, except the later, stricter gate here is the commons
itself, so the artifact simply cannot exist in one.

The cheap fix is for `certify` to schema-validate; the cheaper one is for the adapter to refuse (or
truncate-with-a-note) a name it cannot publish, rather than emitting it. Which of those is right
depends on whether the 64-character cap is itself the thing to revisit — a limit that excludes real
operations from a real cloud API may be the defect.

---

## Do these findings generalise? Four APIs, 1,208 records

The findings above were all measured on Cloud Storage. To see which are properties of *that*
description and which are properties of the description layer, three more Google Cloud APIs went
through the same pipeline, unmodified.

| corpus | records | certified | refused | unpublishable (finding 12) |
|---|---:|---:|---:|---:|
| `storage v1` | 125 | 125 | 0 | 0 |
| `cloudresourcemanager v3` | 28 | 28 | 0 | 0 |
| `iam v1` | 48 | 48 | 0 | 7 |
| `compute v1` (734 paths) | **1007** | 1007 | 0 | 5 |
| **total** | **1208** | **1208** | **0** | **12** |

**The description-layer path holds at SDK scale.** Compute v1 — the largest surface Google
publishes — compiles to 1007 records with nothing refused and everything certified, no modification
to the toolchain.

**Finding 9 generalises completely.** Counting a create as naming its resource outside the body only
when a path or required-query parameter matches the resource noun in its `operationId`:

> **131 of 131** create-shaped operations across all four APIs name the new resource **in the request
> body**. Not one names it in the URL.

So the `body-field` key part that `b689ce5` added is not a GCS accommodation — without it, the
`ensures` clause is inexpressible for every create in every one of these APIs.

**Finding 11 generalises completely.** Across **1,164 operations** in four APIs, **zero** document a
404 — or any non-2xx. Discovery describes success and nothing else. So every path-parameterised
GET/DELETE in any Discovery-derived corpus carries the unsatisfiable example finding 11 describes;
it is a property of the format, not of one API.

**Finding 12 is scale-dependent**, which is why Cloud Storage never showed it: its longest
`name_hint` is 50 characters, comfortably inside the cap. Only the deeply-nested APIs reach it.

## Reproducing

Findings 1–3 need no credentials and no network.

```bash
# finding 1 — hand-authored OpenAPI 3, no converter, no vendor
python3 openapi_ingest.py <fixture-with-requestBody>.json --out /tmp/a
grep -ci content-type /tmp/a/*                     # 0

# finding 2 — two fixtures differing only in the response content-type key
python3 openapi_ingest.py <fixture-application-json>.json --out /tmp/b1   # 4 notes
python3 openapi_ingest.py <fixture-any-star>.json      --out /tmp/b2   # 0 notes before the fix

# the */* provenance — swagger2openapi alone, no Google input
node -e "require('swagger2openapi').convertObj({swagger:'2.0',info:{title:'p',version:'1'},
  paths:{'/s':{get:{operationId:'g',responses:{200:{description:'ok',
  schema:{type:'object',properties:{state:{type:'string'}}}}}}}}},{patch:true,warnOnly:true},
  (e,o)=>console.log(Object.keys(o.openapi.paths['/s'].get.responses['200'].content)))"
# -> [ '*/*' ]

# findings 2, 3, 5 at scale — needs the converted Cloud Storage description
python3 openapi_ingest.py storage.v1.openapi3.bearer.json --out /tmp/gcs 2>&1 | tee /tmp/gcs.log
grep -c "optional query param" /tmp/gcs.log         # 877
grep -c "response body not projected" /tmp/gcs.log  #  69   (after the fix; 0 before)
ls /tmp/gcs/*.v0.2.json | wc -l                     #  81

# finding 8 — fully hermetic: the in-repo fake service, no cloud, no credentials
python3 tooling/fake-service/fake_service.py --port 18879 &
python3 openapi_ingest.py \
    evolution/gcp-sdk-poc/repro/finding-8-optional-projection.openapi.json \
    --out /tmp/f8 --verify-against http://127.0.0.1:18879
# getAbsentBody   observation-gate=FAIL          <- correct
# getabsentalpha  live=OBSERVED+schema-checked   <- from a 401, example is None
# getabsentbeta   live=OBSERVED+schema-checked   <- ditto
python3 -c "import json;print(json.load(open('/tmp/f8/getabsentalpha.v0.2.json'))['examples'][0]['result'])"
# -> {'kind': 'variant', 'tag': 'None'}
```

Findings 9 and 10 need the generated corpus (regenerate it with the pipeline above); neither needs
credentials, because nothing is executed live.

```bash
# finding 9 — the corpus as shipped approves a use-after-delete
#   a plan of [buckets.delete(b), buckets.get(b)] over records with empty refinements
nl-validator check-plan --plan use-after-delete.json --records records/storage-v1
# -> PLAN-SOUND   (exit 0)
#
# then author the two refinements shown above onto buckets.get / buckets.delete, rehash
# (`nl-validator hash <rec> --kind function-record`), and re-check the same plan:
# -> REJECTED  step 2: requires bucket("…") = exists, but it is absent (step 1 …)
# a create -> verify -> delete plan over the same records:
# -> UNVERIFIABLE  2 requirement(s) nothing establishes   (buckets.insert cannot declare `ensures`)

# which creates can name what they create?  (9 of 9: in the body)
python3 - <<'PY'
import json; s=json.load(open("specs/storage.v1.openapi3.normalized.json"))
for path,item in s["paths"].items():
    for m,op in item.items():
        if m!="post" or not isinstance(op,dict) or "requestBody" not in op: continue
        if not any(k in op["operationId"] for k in ("insert","create")): continue
        print(op["operationId"], "path=", [p["name"] for p in op.get("parameters",[])
                                           if p.get("in")=="path"])
PY

# finding 10 — a goal one record satisfies exactly, against a node holding records AND bodies
nl-validator assemble --node http://127.0.0.1:8010 --max-stages 2 \
    --goal evolution/gcp-sdk-poc/repro/finding-10-goal-bucket-location.json
# -> discovered 120 candidate(s); arity-pruned 65, fetching 55 …
# -> NO PIPELINE            (every candidate is effectful; the purity gate rejects all of them)

# the same record reproduces the goal's output with no grants, no secrets, no network
nl-validator run storage_buckets_getlocation.v0.2.json --records .
# -> example 0 PASS {"kind":"variant","payload":{"kind":"string","value":"US"},"tag":"Just"}
```

Loading the node for finding 10 needs the **bodies** as well as the records — the pipeline emits only
the latter, so append the `body-*.json` ASTs to the JSONL (the node detects `kind: body` itself).

The live provisioning step needs a GCP project and `gcloud auth print-access-token`; it is not
required for any finding above.
