# The worked example, hand-traced from needs list to receipt

The glossary's web service, provisioned for a production profile on Google Cloud, followed through every step
of the mechanism with every record written out under [`records/`](records/) (addresses in
[`addresses.md`](addresses.md)). The records were written, hashed, signed and schema-checked by
[`build.py`](build.py), which also re-performs the checks this trace makes by hand (coverage, wire types,
derived names, the planner's waves, address resolution); the trace is the reasoning, the script is the proof
that the reasoning and the records agree. Step numbers refer to [`../algorithms.md`](../algorithms.md).

Operation records are stand-ins for the OpenAPI adapter's output for the named Google Cloud API operations.
The attester is a throwaway design-phase identity. Nothing was provisioned.

---

## 0. What is on the node before anything happens

**Kinds** (four `kind_` records): `network` {private_service_access, subnets, nat}; `relational-database`
{engine: Postgres | Mysql, major, ha, regions, encryption_at_rest, columnar_engine}; `object-store`
{versioning, public_read}; `container-service` {min_instances, max_concurrency}.

**Port types** (five `type_` artifacts): `network-ref` {name, self_link}; `connection-endpoint`
{instance, host, port}; `secret-reference` {store, name, version}; `bucket-ref` {name}; `service-url` {url}.
Each carries the resource's *name* beside whatever link or address it also carries, so a recipe can state a
dependency on a wired resource by name (section 5.5 below).

**Operations** (29 `fn_` stand-ins): the create, read and delete operations of Compute networks and global
addresses, Service Networking connections, Cloud SQL instances and users, Secret Manager secrets and versions,
AlloyDB clusters and instances, Cloud Storage buckets, and Cloud Run services.

**Dishes** (five, each with its recipe), in publication order:

| dish | kind | provides | inputs | outputs |
|---|---|---|---|---|
| `vpc-psa` | network | private_service_access = true, subnets = 0, nat = false | name; project | network: network-ref |
| `cloudsql-pg15-ha-2r` | relational-database | Postgres, 15, ha, 2 regions, encrypted, no columnar engine | name; project, 2 regions; wire network | endpoint: connection-endpoint; credential: secret-reference |
| `alloydb-pg15-ha-2r` | relational-database | the same, with a columnar engine | the same | the same |
| `gcs-versioned-private` | object-store | versioning = true, public_read = false | name; project, region | bucket: bucket-ref |
| `cloudrun-min1-c80` | container-service | min_instances = 1, max_concurrency = 80 | name; project, region; wires network, database, credential, assets | url: service-url |

**Attestations.** Each dish has one passing provisioning attestation (`records/attestation-*.json`), signed
by the attester, naming its harness plate, test instance, test receipt and test scrub receipt, all written out
(`records/harness-*.json`). Section 8 below traces one of them.

**Trust policy.** The operator's policy trusts the dishes' publisher and the attester as roots. Under
algorithm 1 every dish is therefore `offered`: trusted publisher, and a passing attestation from a trusted
attester, with no retiring statement.

---

## 1. The needs list (`records/needs-list-webshop.json`)

Four needs, as the glossary states them, with the predicates over the kinds' attributes:

| need | kind | constraints | wires |
|---|---|---|---|
| `api-network` | network | private_service_access = true | — |
| `primary-db` | relational-database | engine = Postgres ∧ major ≥ 15 ∧ encryption_at_rest = true | lives-in → api-network |
| `assets` | object-store | versioning = true ∧ public_read = false | — |
| `api` | container-service | min_instances ≥ 1 ∧ max_concurrency ≥ 80 | lives-in → api-network; reads-from → primary-db; reads-from → assets |

The list names no profile and no port. A made-from statement (`records/statement-made-from.json`) records that
it was copied from an October draft without the object store (`records/needs-list-webshop-draft.json`); the
statement is a separate record, so the list's address is unaffected.

## 2. The profile (`records/profile-production.json`)

`production`: relational-database must satisfy ha = true ∧ regions ≥ 2; container-service must satisfy
min_instances ≥ 1; no kind may be shared between needs (the `sharing` list is empty, the safe default).

## 3. The selector (algorithm 3)

**Step 1, order the needs.** By wires: `api-network`, then `assets` and `primary-db` (each wires only to
`api-network` or nothing; ties by name), then `api`.

**Step 3, candidates.** Evaluating each need's predicate, conjoined with the profile's predicate for its kind,
on every offered dish's `provides`:

| need | offered dishes of the kind | need predicate | profile predicate | candidates |
|---|---|---|---|---|
| `api-network` | vpc-psa | true (psa = true) | none for network | vpc-psa |
| `primary-db` | cloudsql-pg15-ha-2r, alloydb-pg15-ha-2r | both true (Postgres, 15 ≥ 15, encrypted) | both true (ha, 2 ≥ 2) | both |
| `assets` | gcs-versioned-private | true | none for object-store | gcs-versioned-private |
| `api` | cloudrun-min1-c80 | true (1 ≥ 1, 80 ≥ 80) | true (1 ≥ 1) | cloudrun-min1-c80 |

Ports fit: `primary-db`'s wire `lives-in → api-network` needs a wire input of type network-ref on the
candidate, fed by vpc-psa's output `network` (network-ref): both database dishes have exactly one such input.
`api`'s three wires: `network` ← vpc-psa.network; `database` ← the database serving's `endpoint`
(connection-endpoint) and `credential` ← its `credential` (secret-reference), both carried by the one
`reads-from → primary-db` wire; `assets` ← gcs.bucket (bucket-ref). Every wire input of cloudrun-min1-c80 is
fed exactly once.

**Step 5, rank.** For `primary-db` two candidates remain. Both have one trusted attestation; no preference
rules are in force; publication order decides: cloudsql-pg15-ha-2r was published first. (Had the consigliere
contributed a rule "prefer a columnar engine" or "lower cost", it would have decided here, as an input.)

**Step 4, sharing.** No kind is shareable in this profile; every need gets its own serving.

**Step 7, freeze.** The plate (`records/plate-webshop-production.json`):

- servings `s1` vpc-psa, `s2` cloudsql-pg15-ha-2r, `s3` gcs-versioned-private, `s4` cloudrun-min1-c80;
- coverage api-network → s1, primary-db → s2, assets → s3, api → s4;
- five port bindings: (primary-db, lives-in) s1.network → s2.network; (api, lives-in) s1.network → s4.network;
  (api, reads-from primary-db) s2.endpoint → s4.database and s2.credential → s4.credential; (api, reads-from
  assets) s3.bucket → s4.assets;
- no overrides; an empty cleanup manifest.

## 4. The coverage check (algorithm 2), as the provisioning command will repeat it

1. Every need once: four coverage entries, four needs, four distinct servings. ✓
2. Kinds: s1 is a network dish, s2 a relational-database dish, s3 an object-store dish, s4 a
   container-service dish, each equal to its need's kind. ✓
3. Predicates: as in the selector's table; every conjunction evaluates to true on the pinned dish's provides.
   The alloydb dish is not on the plate, so its extra attribute plays no part. ✓
4. Sharing: no serving covers two needs. ✓
5. Wires: each of the four needs-list wires has at least one plate entry; each entry's input serving covers
   the need and its output serving covers the target; the five type pairs are byte-equal canonical type
   expressions (network-ref = network-ref, connection-endpoint = connection-endpoint, secret-reference =
   secret-reference, bucket-ref = bucket-ref). ✓
6. Every wire input fed exactly once: s2.network once; s4.network, s4.database, s4.credential, s4.assets once
   each; s1 and s3 have no wire inputs. ✓
7. No carried servings. ✓

COVERED.

## 5. The instance (algorithm 4; `records/instance-webshop-g7.json`)

Lineage `acme/webshop/production`; the lineage's latest receipt on the node has generation 6 (not written
out here), so this is generation 7. The organization's naming rule is `<product>-g<generation>-<need>`; its
placement rule puts production in project `acme-webshop-prod`, regions `europe-west1` and `europe-west4`.

| serving | placement | name inputs | derived names (recipe `derive` rules) |
|---|---|---|---|
| s1 | project | name = `webshop-g7-api-network` | `-psa` → `webshop-g7-api-network-psa` |
| s2 | project, regions [europe-west1, europe-west4] | name = `webshop-g7-primary-db` | `-cred` → `webshop-g7-primary-db-cred`; `-replica` → `webshop-g7-primary-db-replica` |
| s3 | project, region europe-west1 | name = `webshop-g7-assets` | — |
| s4 | project, region europe-west1 | name = `webshop-g7-api` | — |

Every name the kitchen will send is in this record, including the three derived ones, each marked with the
suffix it was derived by. The provisioning command's instance check (algorithm 5, step 3) recomputes
`webshop-g7-primary-db` + `-cred` and finds it equal to the recorded value, and so on for the other two.

## 5.5 Expansion (algorithm 5, step 5)

Each serving's recipe is flattened into steps with plan-time bindings grounded from the instance. Ten steps:

| step | operation | grounded arguments (abbreviated) | requires | ensures |
|---|---|---|---|---|
| s1/net-insert | compute.networks.insert | project; `{"name":"webshop-g7-api-network","autoCreateSubnetworks":false}` | — | network(webshop-g7-api-network) exists |
| s1/addr-insert | compute.globalAddresses.insert | project; body with name `…-psa`, network = *later* (net-insert.self-link) | network(…api-network) exists | global-address(…-psa) exists |
| s1/psa-connect | servicenetworking…connections.create | parent; body with network = *later*, reservedPeeringRange `…-psa` | network exists; global-address exists | psa-connection(…api-network) exists |
| s2/inst-insert | sqladmin.instances.insert | project; body with name `webshop-g7-primary-db`, region europe-west1, privateNetwork = *later* (s1 self-link) | psa-connection(**webshop-g7-api-network**) exists | sql-instance(…primary-db) exists |
| s2/replica-insert | sqladmin.instances.insert | project; body with name `…-replica`, region europe-west4, master `…primary-db` | sql-instance(…primary-db) **ready** | sql-instance(…-replica) exists |
| s2/secret-create | secretmanager…secrets.create | project; `…-cred`; body | — | secret(…-cred) exists |
| s2/secret-version | secretmanager…secrets.addVersion | project; `…-cred`; **Secret** | secret(…-cred) exists | secret-version(…-cred) exists |
| s2/user-insert | sqladmin.users.insert | project; `…primary-db`; `{"name":"app","password":"{{secret}}"}` | sql-instance ready; secret-version exists | sql-user(…primary-db, app) exists |
| s3/bucket-insert | storage.buckets.insert | project; body with name `webshop-g7-assets`, location, versioning, no public access | — | bucket(…assets) exists |
| s4/svc-create | run…services.create | project; region; `webshop-g7-api`; body with network, DB_HOST, DB_PORT, DB_PASSWORD (secret reference), ASSETS_BUCKET, some *later* | network exists; sql-instance(**…primary-db**) ready; secret-version(…-cred) exists; bucket(…assets) exists | run-service(…api) exists |

The two bold keys show the wire mechanism: s2's recipe says `requires psa-connection(wire network.name)`, and
the network-ref type carries the name, bound in s1's recipe from its own name input, so the key grounds to
`webshop-g7-api-network` at plan time even though the network's *link* is known only at kitchen time. Likewise
s4's `requires sql-instance(wire database.instance) ready` grounds to `webshop-g7-primary-db`. Every key in
every step grounded; the `{{…}}` placeholders occur only in argument bodies, never in keys.

## 6. The planner (algorithm 6; `records/plan-webshop-g7.json`)

**Satisfaction edges.** net-insert → addr-insert, net-insert → psa-connect, addr-insert → psa-connect
(network and global-address ensures); psa-connect → inst-insert (psa-connection); secret-create →
secret-version (secret); secret-version → user-insert and → svc-create (secret-version); bucket-insert →
svc-create (bucket); net-insert → svc-create (network).

**Wait step.** replica-insert, user-insert and svc-create each require `sql-instance(webshop-g7-primary-db)
ready`; no step ensures `ready`, but inst-insert ensures `exists` for that resource. The planner inserts one
wait step from the cloudsql recipe's readiness declaration for class sql-instance: probe
sqladmin.instances.get(project, webshop-g7-primary-db), ready when `state = "RUNNABLE"`, limit 900 s; edges
inst-insert → wait → {replica-insert, user-insert, svc-create}.

**Data edges.** addr-insert, psa-connect, inst-insert and svc-create consume net-insert's self-link;
svc-create consumes inst-insert's ip and secret-version's version. All already implied by the satisfaction
edges above.

**Assumptions.** None: every `requires` is ensured by a step in the plan. (The project's existence and the
enabled APIs are not modelled as resources in this example; a real recipe would state them as requires on a
`project` class, which would become the plan's probed assumptions.)

**Waves** (Kahn's algorithm, ties by serving id then step id):

| wave | steps |
|---|---|
| 0 | s1/net-insert, s2/secret-create, s3/bucket-insert |
| 1 | s1/addr-insert, s2/secret-version |
| 2 | s1/psa-connect |
| 3 | s2/inst-insert |
| 4 | wait until sql-instance(webshop-g7-primary-db) is RUNNABLE |
| 5 | s2/replica-insert, s2/user-insert, s4/svc-create |

Eleven plan steps (ten operations, one wait), six waves, no assumptions.

**check-plan** (algorithm 5, step 7), symbolically over the flat sequence: every requires is discharged by
an earlier ensures in the same sequence (the wait step's ensures `ready` discharges the three `ready`
requirements); no requires is contradicted; the assumption list is empty, so nothing is testimony.
PLAN-SOUND. The plan is published as a `plan_` artifact and cited by the receipt.

One observation for the chef, not a defect: svc-create does not require `sql-user(…, app) exists`, so the
service may be created before its database user; the application connects at run time, so this is harmless
here, and a recipe that wanted the stronger order would add the requires.

## 7. The kitchen and the receipt (algorithm 5, steps 8–10; `records/receipt-webshop-g7.json`)

Waves run in order. In wave 1 the kitchen generates a 32-character value of the `secret` type and passes it to
secrets.addVersion (the plan shows the argument as the variant `Secret`); in wave 5 the same value is
rendered into the users.insert body at the `{{secret}}` placeholder by the effect boundary; the trace, had it
been retained, would carry the placeholder at both sites and the value nowhere. In wave 4 the kitchen polls
instances.get every 15 s until `state` reads RUNNABLE, within 900 s.

The receipt records, per serving, the dish that stands there, its recipe, status `provisioned`, its output
ports and the ground resources it ensured:

| serving | outputs | resources ensured |
|---|---|---|
| s1 | network = {name: webshop-g7-api-network, self_link: …/global/networks/webshop-g7-api-network} | network, global-address (…-psa), psa-connection |
| s2 | endpoint = {instance: webshop-g7-primary-db, host: 10.42.0.3, port: 5432}; credential = {store: secretmanager, name: webshop-g7-primary-db-cred, version: projects/…/secrets/…-cred/versions/1} | sql-instance (×2), secret, secret-version, sql-user(…, app) |
| s3 | bucket = {name: webshop-g7-assets} | bucket |
| s4 | url = {url: https://webshop-g7-api-….a.run.app} | run-service |

The credential output is a secret reference, not a value; nothing in the receipt could hold the password,
because no value-expression kind exists for the `secret` type. The receipt cites the plate, the instance, the
plan, lineage `acme/webshop/production` and generation 7; it supersedes nothing (a new generation is a new
environment). It is the system's whole knowledge of this environment.

## 7.5 Scrub (algorithm 7; `records/plan-webshop-g7-scrub.json`, `records/receipt-webshop-g7-scrub.json`)

Given the receipt alone. Policy allows deletes for this lineage. The retire steps of the four recipes are
expanded with names from the instance; cross-serving order comes from the provisioning dependencies reversed
(s4 depended on s2, s3 and s1; s2 on s1). The planner's waves:

| wave | steps |
|---|---|
| 0 | s4/svc-delete |
| 1 | s2/replica-delete, s2/secret-delete, s3/bucket-delete |
| 2 | s2/inst-delete |
| 3 | s1/psa-delete |
| 4 | s1/addr-delete |
| 5 | s1/net-delete |

Within s1 and s2 the order comes from the retire steps' own requires (the instance is deleted only after its
replica is absent; the network only after the connection and the address range are absent). Each delete is
followed at run time by a wait until the class's probe answers not-found, within the class's limit. The scrub
receipt supersedes the provisioning receipt and marks every resource `absent`; the environment is scrubbed.

## 8. One test provisioning (algorithm 8), for `cloudsql-pg15-ha-2r`

The chef submitted the dish with a harness plate (`records/harness-plate-cloudsql-pg15-ha-2r.json`): the dish
under test as `s2`, fed by `vpc-psa` as `s1` through the one wire `network`; its harness needs list has two
unconstrained needs; the attester's harness profile has no constraints. Coverage passes. The attester's
instance generator names the test lineage after the dish's address and marks every name with `t-`
(`records/harness-instance-cloudsql-pg15-ha-2r.json`). Expansion and the planner give six waves, with the
same wait on the instance's RUNNABLE state. The run's receipt and the scrub receipt that followed are written
out; the attestation (`records/attestation-cloudsql-pg15-ha-2r.json`) names dish, recipe, harness plate,
instance, result `pass`, both receipts and the times, and is signed by the attester; `build.py` verifies the
signature over the JCS form with `signature` removed, exactly as a certification is verified.

The same was done for the other four dishes; the Cloud Run harness is the largest (four servings, because the
service has four wire inputs), and the AlloyDB harness shows two wait steps (cluster READY, then primary
instance READY).

## 9. What this trace demonstrates against the charter

- R2, R14: coverage is predicate evaluation on a typed value; the alloydb dish's extra attribute is harmless.
- R3, R11, R13a: names, placements and wires are the only inputs; the plate and the instance together hold
  everything the command needs, and the instance holds every name including derived ones.
- R15, R16: the order of work came from the recipes' refinements, the wait is a declared step, and the plan
  rests on no assumption.
- R18, decision 31: the credential crosses the receipt as a reference; the value has no serializable form.
- R19, R20: scrub was planned from the receipt alone, in reverse dependency order.
- R5, R6, decision 32, 36: every offered dish has a signed attestation whose inputs are written out, so any
  attester could reproduce the test.

The phase-2 gate ("the glossary's web service hand-traced from needs list to receipt with every record written
out") is met by this document and `records/`.
