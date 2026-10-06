# A whole cloud ecosystem provisioned, operated and torn down through records and checked plans

- **Status:** absorbed (2026-10-06). The module's language-level conclusions are in the tree:
  the awsJson adapter [`tooling/nl-ingest-smithy`](../../tooling/nl-ingest-smithy/) (GW19 in
  `spec/expressiveness.md`), the `sleep` builtin, secret placeholders in request bodies, and the
  evaluator's 60 s control-plane timeout. The provisioning vocabulary itself (96 hand-authored
  records over Cloud Control, ECS, SSM and ECR) is the operator's and is published to the commons
  only in its account-free subset (41 records, with bodies, traces and signed certifications, on Arca); the remainder stays with the
  operator by the `aws-sdk-poc` ARN boundary.
- **Author:** Keith Sprochi <kds1729@gmail.com>
- **Dates:** first published 2026-10-06, last updated 2026-10-06
- **Scope:** `tooling/nl-ingest-smithy` (new), `tooling/fake-service` (a `/rpc` surface),
  `tooling/validator` (`sleep`; body placeholders; timeout), `spec/evaluation.md`,
  `spec/expressiveness.md` (GW19); read against `spec/world-state.md` (`check-plan`) and
  `evolution/aws-sdk-poc`.
- **Provenance:** upstream commit `6f5c9a5` (the tree at the end of the day); AWS service models
  from `aws/api-models-aws` at commit `7eb6ab98` (ECS 2014-11-13, ECR 2015-09-21, Cloud Map
  2017-03-14, Global Accelerator 2018-08-08, Cloud Control 2021-09-30; `smithy-cli` 1.73.0 was
  used only to measure that the awsJson models do NOT project to OpenAPI); one AWS account, two
  regions (us-east-1, us-west-2), everything built and destroyed within one day
  (2026-10-06 ~07:30–12:30 EDT); a local SigV4-signing proxy as the records' `base`.
- **Resolution:** absorbed — see Status. No proposals: every language-level question this run
  raised was settled by measurement in-tree the same day.

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
| Repository | `6f5c9a5` |
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

## Open questions

- A poll budget as a *time* rather than a step count (finding 11): the records pass a depth;
  Cloud Control's `RetryAfter` hint in the `ProgressEvent` is the principled input and is
  currently ignored.
- Observation probes (`check-plan --probe`) against the ecosystem's `requires` were not run;
  the teardown audit was done with the AWS API directly. Running the probes would close that gap
  in-language.
- The ARN boundary is coarse: 55 of the 96 records are held back only because an observed
  value in an example or a trace carries an ARN or the account id; a redaction rule for
  observed values would let the whole vocabulary publish.

## Reproducing

The adapter's offline and fake-service results need no credentials (`tooling/nl-ingest-smithy`,
`python3 -m unittest discover -s tests`). The live results need an AWS account, a signing proxy,
and a permissions boundary covering the services named above; the operator's builders
(`build_*_records.py`, `make_plan.py`, `run_plan.py`) are not in this repository.
