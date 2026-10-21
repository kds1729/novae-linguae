# A whole cloud ecosystem provisioned, operated and torn down through records and checked plans

- **Status:** absorbed (2026-10-06; the three open questions closed 2026-10-17, below). The
  module's language-level conclusions are in the tree: the awsJson adapter
  [`tooling/nl-ingest-smithy`](../../tooling/nl-ingest-smithy/) (GW19 in
  `spec/expressiveness.md`), the `sleep` builtin, secret placeholders in request bodies, the
  evaluator's 60 s control-plane timeout, and — from the follow-up — a real clock behind `now`,
  the outbound `eval --redact` rule, composed probes in `check-plan`, and a Cloud Control stand-in
  in the fake service. The provisioning vocabulary (96 hand-authored records over Cloud Control,
  ECS, SSM and ECR, now 128 with the pacing and probe records) is the operator's; since
  2026-10-17 the WHOLE vocabulary is on Arca in redacted form (bodies, traces, signed
  certifications) — the `aws-sdk-poc` ARN boundary is kept by redaction rather than by holding
  records back.
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-06, last updated 2026-10-17
- **Scope:** `tooling/nl-ingest-smithy` (new), `tooling/fake-service` (a `/rpc` surface; 2026-10-17
  a `CloudApiService.*` Cloud Control stand-in with request tokens and `RetryAfter`),
  `tooling/validator` (`sleep`; body placeholders; timeout; 2026-10-17 `now` as the epoch-ms
  clock, `eval --redact`, `check-plan --probe` over composed probe bodies), `spec/evaluation.md`,
  `spec/world-state.md` (probes, audits), `spec/expressiveness.md` (GW19); read against
  `evolution/aws-sdk-poc`.
- **Provenance:** upstream commit `982140f` (the tree at the end of the first day) and the
  2026-10-17 follow-up commit; AWS service models from `aws/api-models-aws` at commit `7eb6ab98`
  (ECS 2014-11-13, ECR 2015-09-21, Cloud Map 2017-03-14, Global Accelerator 2018-08-08, Cloud
  Control 2021-09-30; `smithy-cli` 1.73.0 was used only to measure that the awsJson models do NOT
  project to OpenAPI); one AWS account, two regions (us-east-1, us-west-2), everything built and
  destroyed within one day (2026-10-06 ~07:30–12:30 EDT); a local SigV4-signing proxy as the
  records' `base`. The follow-up ran against the same account, empty, on 2026-10-17 (two
  throw-away VPCs created and deleted; nothing else touched).
- **Resolution:** absorbed — see Status. No proposals: every language-level question this run
  raised was settled by measurement in-tree, on the day or in the follow-up.

## Summary

Starting from a bare account, the following was provisioned by evaluating certified records
sequenced in `check-plan`-verified plans, with no infrastructure tool and no console: in each of
two regions a VPC, two public subnets, routing, security groups, IAM roles, an ECR repository, a
log group, an ECS cluster on EC2 capacity (launch template, auto-scaling group, capacity
provider, with Fargate attached as an alternative), an application load balancer with blue/green
target groups, a Postgres database (primary in one region, cross-region read replica in the
other), and a service (first a Django scaffold, then a Node.js hello-world) deployed with ECS's
native canary strategy; globally, an accelerator over both load balancers and a CloudFront
distribution over the accelerator. A canary deployment (v1 → v2) was observed from outside
shifting 10 % of traffic, baking, and completing. Every resource was then deleted by the
inverse records, in plans the same checker verified for ordering, and an independent audit (the
AWS API, not the records) found nothing of the ecosystem's left. The uncomfortable parts: the
build surfaced one language gap (no way to pace a poll — `sleep`), one evaluator limit (a 15 s
HTTP timeout too short for control planes), and one placeholder gap (credentials in request
bodies); the records surfaced a long list of Cloud Control behaviours a description never
states (below); and the first run of one plan minted a duplicate CloudFront distribution because
its identity field is nested where the finder looked flat.

## Provenance of the inputs

| Input | Pinned at |
|---|---|
| Repository | `982140f` |
| `aws/api-models-aws` | `7eb6ab98` (five models; sha256 prefixes in the operator's `PINNED.json`) |
| AWS account / regions | one account; `us-east-1` (writer) and `us-west-2`; Global Accelerator's control plane in `us-west-2`; CloudFront via `us-east-1` |
| Signing boundary | a local SigV4 proxy routing `http://127.0.0.1:9099/<service>/<region>/` to the signed upstream (aws-sdk-poc finding 5) |
| Permissions | an IAM permissions boundary as the ceiling, grown by the provisioner itself through `CreatePolicyVersion` (9 versions over the day); inline user policies within the 2,048-byte per-user total |
| Container images | `node:22-alpine` + `pg`; `python:3.12-slim` + Django 5 for the scaffold; built on a separate host |

## What was measured

Everything below is from the plans' per-step traces and the adapter's run reports.

**The adapter, offline** (`smithy_ingest.py <model> --out …`): ECS 72 of 77 operations plan
(106 records licensed), Global Accelerator 51/56, Cloud Control 7/8, ECR 46/58, Cloud Map 27/30.
The `readonly` trait is present on 29 of ECS's 77 operations and on **none** of the other four
services' 152. The Smithy→OpenAPI projection fails on all five models (awsJson1_0/1_1 are not
projectable); EKS (restJson1) projected and compiled 52/70 before the design moved to ECS.

**The adapter, live** (same day, an otherwise empty account): 258 records certified and
offline-replayed from 221 calls, nothing created. Every rejection row held — no service accepted
`{}` where its model requires a member; ECS answers `CreateService {}` with
`InvalidParameterException` *before* checking `ecs:CreateService`, while `DeleteCluster {}`
answers `AccessDeniedException` first. `ListServices`/`ListTasks`/`ListContainerInstances` at
`{}` answer `ClusterNotFoundException` (the model calls `cluster` optional; the implied default
cluster does not exist). `ListServicesByNamespace {}` answers `500`. Cloud Map's
`DiscoverInstances*` answer `UnknownOperationException` at the service's ordinary host (their
model carries `smithy.api#endpoint hostPrefix "data-"`).

**The ecosystem**: 96 hand-authored records; region plans of 30 steps (build) and 32/28 steps
(teardown), a 4-step global plan, a 1-step CDN plan, 2-step deploy plans; 36 plan runs. Every
plan `PLAN-SOUND` before running; a deliberately misordered network plan refused at its third
step; fragments run alone refused until their cross-plan dependencies were stated as
assumptions. A full build re-run of a complete region found every resource and changed nothing
(~30 s). Region 2 was the identical plan with a different config (`base`, CIDR, AZs) and ran in
~12 min including the instance boot. The canary deploy: plan issued 13:02:19Z; samples of 20
requests every 20 s read 20/0 (v1/v2) until 13:03:32, mixed (18/2, 13/7, 20/0, 19/1, 19/1,
19/1) from 13:03:53, 0/20 at 13:06:01. Teardown: the global plan 4/4, us-west-2 28/28, us-east-1
32/32 (one re-run each for a poll budget and an idempotency case, below); an API-level audit of
both regions and the global services afterwards listed only the account's pre-existing default
resources. Peak standing cost during the day ≈ $135/month-equivalent; total spend for the day
on the order of a few dollars.

**Cloud Control behaviours that no description states** (each hit live, each now handled by a
record): (1) `ListResources` paginates with `NextToken`; (2) some types list only under a
parent via `ResourceModel` (`Listener` needs `LoadBalancerArn`, `ListenerRule` needs
`ListenerArn`, `SubnetRouteTableAssociation` accepts one); (3) a `FAILED` create can leave a
partial resource — an `AWS::IAM::InstanceProfile` without its role, an `AWS::IAM::Role` without
its managed policy — with no rollback; (4) `$Latest` is refused as a launch-template version
("CloudFormation does not support using $Latest…"), a version number is required; (5)
`AWS::EC2::SecurityGroupIngress` is identified by an `sgr-` id, not the composite the CFN schema
implies; (6) CloudFront's `Comment` (its only name-like field) is nested under
`DistributionConfig`, and an update must restate the `ViewerCertificate` block or is refused;
(7) `EmptyOnDelete` on `AWS::ECR::Repository` is honored by CloudFormation stacks, not by a
Cloud Control delete — images must be removed through the ECR API first; (8) a Cloud Control
delete of `AWS::RDS::DBInstance` takes a **final snapshot**, which keeps billing after the
instance is gone; (9) RDS Postgres refuses read replicas while `ManageMasterUserPassword` is on,
and a cross-region replica of an encrypted source must name a destination-region KMS key; (10)
IAM propagation of a new policy version took up to ~30 s to become effective; (11) Global
Accelerator endpoint groups and CloudFront disables take 4–10 minutes — longer than a 60-poll
budget at 3 s — the record returns `None` and a re-run finds the completed resource.

## What is argued

- **Names, not placeholders.** The decisive design choice was keying every record by the
  client-chosen name it would later be found by (a `Name` tag, a client-named identifier, or a
  field on the parent) and resolving server-assigned ids *in-language* through the finders. That
  is what let plans stay literal, let `check-plan` see real `requires`/`ensures` dependencies, and
  made every step idempotent. Threading server ids between steps through the plan would have
  needed a dataflow extension the plan format deliberately lacks.
- **`ensure` means *in the desired state*, not *exists*.** Finding (3) above shows why an
  existence check alone is unsound: the repair branches (`cc_update` with a JSON patch) are the
  correct shape, and the same shape handles drift later.
- **The description layer is thin for provisioning.** The adapter's 258 records are a vocabulary
  of calls; the 96 hand-authored records are where the semantics live (what to find, what to
  wait for, what counts as done). The honest reading is that an API model licenses the wire, and
  the operator's knowledge of the service writes the record. The eleven behaviours above are the
  measure of that gap.
- **The teardown is the stronger half of the proof.** Building shows the records are
  sufficient; deleting everything, with the same checker verifying the inverse order and an
  independent audit confirming the result, shows they are complete.

## What worked well

`check-plan` refusing a misordered plan and an unstated cross-plan dependency, every time, before
any call; the `{}`-rejection row of the adapter (creates compile without creating, and the rule
held on five real services); the plan runner refusing anything but `PLAN-SOUND`; traces per step
(every failure above was diagnosed from the recorded response, never from the console); the
permissions-boundary self-service (nine widenings, zero console visits after the first); the
secret-placeholder discipline (the database password was set through a record whose body and
trace carry only `{{secret:db_master}}`).

## Defects reported

None against the project beyond what was fixed in-tree the same day (`sleep`, body placeholders,
the 60 s timeout, the adapter's request-refusal rule, effect-mode projections, the host-prefix
note). The duplicate-distribution incident was a record defect (a flat lookup of a nested
field), fixed by `cc_find_nested`; the duplicate was disabled and deleted.

## Follow-up (2026-10-17): the three open questions, closed

Each was a measurement first and a change second. Everything below ran against the real account
(empty since the teardown) through the same signing proxy; the fake service's new Cloud Control
stand-in carried the deterministic halves.

### Measured

**`RetryAfter`, re-read from the 2026-10-06 run traces** (85 step traces polling 92 requests):
37 requests carry the hint. In 24 of them it is ONE fixed instant restated on every poll; in 13
(the long operations — RDS, CloudFront, Global Accelerator) it advances across polls (3–60
distinct values). It first appears on the SECOND status read in 32 of 37 requests (never on the
create/delete response itself), and the fixed-step records polled a mean of 12.7 times per
hinted request, i.e. ~30 s of 3 s polls before the first hint passed. Live on 2026-10-17 (a VPC
create): `IN_PROGRESS` (no hint) → `IN_PROGRESS` (no hint) → wait 3.0 s (default) → hint +10.6 s
→ wait 8.2 s → `SUCCESS`: two polls where the old record made five. A first draft that waited
`max(0, hint − now)` re-polled at 0 ms twice once the hint had passed — a stale hint collapses
the budget — so the rule is: past or absent hint ⇒ the default interval.

**The in-language teardown audit**: 28 probe records (one per world-state class, each the FIND
half of its delete record, `: base name … → int`, all observed live answering 404), an audit plan
of 62 `absent` assumptions over 28 classes derived mechanically from the four teardown plans'
`ensures`, zero steps. `check-plan --probe …` × 28: **62/62 OBSERVED, exit 0** — the account holds
nothing of the ecosystem, decided by the language's own machinery. Negative control: two
throw-away VPCs created (through the paced create), two extra `absent` assumptions → **2
PROBE-REFUTED, exit 1**, the right two; deleted (through the paced delete) → **64/64 OBSERVED**.
The probes are composed records (they apply the vocabulary's finders by `fn_ref`); the checker
had never linked a probe body — the first run failed `cannot apply unresolved fn_ref` — fixed
in-tree (the resolver is installed over the plan's records view).

**Redaction**: of 128 records (96 + 2 pacing + 2 paced mutations + 28 probes), **58** carry the
account's identity in an observed value (55 of the original 96; 3 new probes — listing security
groups returns the default groups' `OwnerId`, and ECR's not-found message names the registry)
and **14 more** change by dependency. Four rules (the account id → `123456789012`; three IAM
unique ids → equal-length placeholders). **72 re-minted in dependency order, 72/72 certify and
replay from the redacted traces**, nothing identity-bearing left in any file. Six of the 41
records published on 2026-10-06 referenced held-back records by address — they were on Arca with
dangling `fn_ref`s (the gate does not resolve `fn_ref` targets; a dark target weakens only its
own candidate) — and now carry superseding re-issues. Three teardown records referenced an
ORPHANED address: `build_teardown_records.py` re-minted the shared `cc_delete` for every group,
so the global group's records pointed at a version the region build overwrote (they ran on
2026-10-06 because the file still existed then); re-linked.

### Argued

- **A poll budget is a depth bound times a service hint.** `depth` stays the termination
  certificate (structural descent); the hint supplies the time. The float timestamp is turned
  into whole seconds in-language (`to_string` → `str_split "."` → `parse_int`) — float→int
  conversion is not a builtin, and this is the honest workaround, recorded rather than pulled.
- **An audit is a plan with no steps.** Assumptions are the place testimony enters; probing all
  of them IS the audit, and `check-plan`'s own verdict vocabulary (OBSERVED / PROBE-REFUTED)
  reports it. The plan schema's `steps.minItems: 1` means an audit cannot be published as a
  `plan_` artifact yet — a monotone loosening, left for a driver.
- **Redaction belongs at the effect boundary, as the mirror of secrets.** `--secret` keeps an
  operator value out of the request record; `--redact` keeps a service-assigned value out of the
  response record. Both act after the live effect and before anything is recorded; equal-length
  placeholders keep length-dependent computations consistent; replay agreeing with the redacted
  result is the proof that the body treated the value opaquely (72/72 here). The post-hoc
  re-mint of already-observed traces is operator machinery (`redact_records.py`), and the
  cascade it needed — re-link dependents, supersede what was published — is the ordinary
  cost_sweep shape.
- **Probes over JSON-RPC are `net.write` by the method rule** (every call is a POST). "Read-only"
  is the operator's declaration through the grant, exactly as `--readonly` is for the adapter;
  the effect name cannot carry it.

### Still open

- Time-based budgets for the delete half of the vocabulary: only `cc_delete_paced` exists; the
  typed `delete_*` records still apply the fixed-step `cc_delete`. Re-linking them is a re-mint
  with re-observation (each example deletes), so it waits for the next build.
- Mid-plan probing and the `plan_` schema loosening remain as `spec/world-state.md` states them.

## Reproducing

The adapter's offline and fake-service results need no credentials (`tooling/nl-ingest-smithy`,
`python3 -m unittest discover -s tests` — the Cloud Control stand-in's asynchronous lifecycle and
`RetryAfter` are pinned there too; `python3 tooling/fake-service/fake_service.py --cc-polls 2
--cc-retry-after 1.0` serves it for a paced poll to run against). The live results need an AWS
account, a signing proxy, and a permissions boundary covering the services named above; the
operator's builders (`build_*_records.py`, `make_plan.py`, `run_plan.py`, and from the follow-up
`build_pacing_records.py`, `build_probe_records.py`, `make_audit_plan.py`, `redact_records.py`)
are not in this repository. The redacted vocabulary on Arca is discoverable by its tags
(`io/network/http/cloudcontrol-create-paced`, `query/lookup/aws-probe-<class>`, …); every record
there replays its worked example offline.
