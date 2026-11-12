#!/usr/bin/env python3
"""Design-phase builder and checker for the worked example (gcp-provisioning-poc, phase 2).

This script is NOT the provisioning mechanism. It writes out every record of the glossary's web service
(kinds, port types, operation stand-ins, dishes with recipes, the needs list, the production profile, the
plate, the instance, the plan, the receipts, and one test provisioning per dish with its signed attestation),
computes real content addresses (BLAKE3 over the JCS form, spec/canonical-serialization.md), signs the
attestations with a throwaway design-phase key, validates every record against design/schemas/ and the spec
sub-schemas, and then CHECKS the hand trace in trace.md: coverage, wire types, the instance's derived names,
the planner's waves, and that every address referenced resolves to a record written here.

Run:  design-venv/bin/python -I build.py        (needs blake3, rfc8785, jsonschema, cryptography)
Out:  records/<file>.json, addresses.md
"""
import base64, copy, json, pathlib, re, sys
import blake3, rfc8785
from jsonschema import Draft202012Validator
from referencing import Registry, Resource
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives import serialization

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / 'records'
SCHEMAS = HERE.parent / 'schemas'
SPEC = HERE.parents[3] / 'spec'
OUT.mkdir(exist_ok=True)

# ----------------------------------------------------------------------------- hashing and signing
def canon(obj) -> bytes:
    return rfc8785.dumps(obj)

def address(prefix, obj, signed=False):
    o = {k: v for k, v in obj.items() if k != 'hash' and not (signed and k == 'signature')}
    return f"{prefix}_{blake3.blake3(canon(o)).hexdigest()}"

SEED = blake3.blake3(b'gcp-provisioning-poc design-phase attester 2026-11-12').digest()
ATTESTER_KEY = Ed25519PrivateKey.from_private_bytes(SEED)
ATTESTER_PUB = ATTESTER_KEY.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
ATTESTER = f"did:nova:{ATTESTER_PUB}"

def sign(obj):
    o = {k: v for k, v in obj.items() if k != 'signature'}
    return 'ed25519:' + base64.b64encode(ATTESTER_KEY.sign(canon(o))).decode()

# ----------------------------------------------------------------------------- schemas
def load(p): return json.loads(pathlib.Path(p).read_text())
module_schemas = {p.name: load(p) for p in SCHEMAS.glob('*.schema.json')}
spec_schemas = {n: load(SPEC / n) for n in ['type-expression.schema.json', 'predicate-expression.schema.json',
                                             'value-expression.schema.json', 'function-record.v0.2.schema.json']}
reg = Registry()
for name, sch in {**module_schemas, **spec_schemas}.items():
    reg = reg.with_resource(sch['$id'], Resource.from_contents(sch)).with_resource(name, Resource.from_contents(sch))
validators = {n: Draft202012Validator(s, registry=reg) for n, s in {**module_schemas, **spec_schemas}.items()}

def validate(obj, schema):
    errs = sorted(validators[schema].iter_errors(obj), key=lambda e: list(e.path))
    if errs:
        e = errs[0]
        sys.exit(f"SCHEMA FAIL {schema} at {'/'.join(map(str, e.path))}: {e.message[:400]}")

# ----------------------------------------------------------------------------- the record store
RECORDS = {}   # address -> (file, obj)
FILES = []     # (file, address, what)

def emit(file, prefix, obj, schema, signed=False, what=''):
    obj = copy.deepcopy(obj)
    obj['hash'] = address(prefix, obj, signed)
    if signed:
        obj['signature'] = sign(obj)
    validate(obj, schema)
    (OUT / f"{file}.json").write_text(json.dumps(obj, indent=1, ensure_ascii=False) + "\n")
    RECORDS[obj['hash']] = (file, obj)
    FILES.append((file, obj['hash'], what))
    return obj['hash']

# ----------------------------------------------------------------------------- small constructors
def S(v): return {"kind": "string", "value": v}
def N(v): return {"kind": "nat", "value": v}
def B(v): return {"kind": "bool", "value": v}
def V(tag): return {"kind": "variant", "tag": tag}
def REC(**fields): return {"kind": "record", "fields": [{"name": k, "value": v} for k, v in sorted(fields.items())]}
def T_REC(**fields): return {"kind": "record", "fields": [{"name": k, "type": v} for k, v in sorted(fields.items())]}
def T(name): return {"kind": "builtin", "name": name}
def T_SUM(*tags): return {"kind": "sum", "variants": [{"tag": t} for t in sorted(tags)]}
def var(n): return {"kind": "var", "name": n}
def lit(v): return {"kind": "lit", "value": v}
def app(op, *args): return {"kind": "app", "op": op, "args": list(args)}
def AND(*xs):
    xs = list(xs)
    return xs[0] if len(xs) == 1 else app("and", xs[0], AND(*xs[1:]))

def b_lit(v): return {"kind": "lit", "value": v}
def b_in(n): return {"kind": "input", "name": n}
def b_in_at(n, i): return {"kind": "input-at", "name": n, "index": i}
def b_der(frm, suf): return {"kind": "derive", "from": frm, "suffix": suf}
def b_wire(n, f): return {"kind": "wire", "name": n, "field": f}
def b_oout(o, out): return {"kind": "operation-output", "operation": o, "output": out}
def b_json(**f): return {"kind": "json", "fields": f}
def b_jlist(*items): return {"kind": "json-list", "items": list(items)}
def b_rec(**f): return {"kind": "record", "fields": f}
def b_secret(n=32): return {"kind": "secret", "generate": {"length": n}}
def b_rkey(i): return {"kind": "resource-key", "index": i}
def k_in(n): return {"kind": "input", "name": n}
def k_der(frm, suf): return {"kind": "derive", "from": frm, "suffix": suf}
def k_wire(n, f): return {"kind": "wire", "name": n, "field": f}
def k_lit(v): return {"kind": "lit", "value": v}
def k_body(arg, field): return {"kind": "body-field", "arg": arg, "field": field}
def REQ(cls, key, st="exists"): return {"kind": "requires", "resource": {"class": cls, "key": key}, "state": st}
def ENS(cls, key, st="exists"): return {"kind": "ensures", "resource": {"class": cls, "key": key}, "state": st}

# ----------------------------------------------------------------------------- 1. port types (type_ artifacts)
TYPES = {}
def emit_type(name, node, what):
    validate(node, 'type-expression.schema.json')
    h = f"type_{blake3.blake3(canon(node)).hexdigest()}"
    (OUT / f"type-{name}.json").write_text(json.dumps(node, indent=1) + "\n")
    RECORDS[h] = (f"type-{name}", node); FILES.append((f"type-{name}", h, what)); TYPES[name] = h
    return h
emit_type('network-ref', T_REC(name=T('string'), self_link=T('string')), 'port type: a network, by name and link')
emit_type('connection-endpoint', T_REC(instance=T('string'), host=T('string'), port=T('nat')), 'port type: a database endpoint, naming its instance')
emit_type('secret-reference', T_REC(store=T('string'), name=T('string'), version=T('string')), 'port type: a secret reference (decision 31)')
emit_type('bucket-ref', T_REC(name=T('string')), 'port type: a bucket, by name')
emit_type('service-url', T_REC(url=T('string')), 'port type: a service URL')
def TREF(name): return {"kind": "ref", "target": TYPES[name]}

# ----------------------------------------------------------------------------- 2. kinds
KINDS = {}
def emit_kind(name, attrs, docs):
    k = {"kind": "kind", "schema_version": "0.1.0", "name": name, "attributes": T_REC(**attrs),
         "attribute_docs": [{"name": n, "doc": d} for n, d in sorted(docs.items())], "supersedes": None}
    validate(k['attributes'], 'type-expression.schema.json')
    KINDS[name] = emit(f"kind-{name}", 'kind', k, 'kind.schema.json', what=f"kind {name}")
emit_kind('network', dict(private_service_access=T('bool'), subnets=T('nat'), nat=T('bool')),
          dict(private_service_access="whether managed services can attach by private address", subnets="how many subnets the network comes with", nat="whether outbound network address translation is provided"))
emit_kind('relational-database', dict(engine=T_SUM('Postgres', 'Mysql'), major=T('nat'), ha=T('bool'), regions=T('nat'), encryption_at_rest=T('bool'), columnar_engine=T('bool')),
          dict(engine="the database engine", major="the engine's major version", ha="whether the service fails over automatically", regions="how many regions hold a copy of the data", encryption_at_rest="whether stored data is encrypted", columnar_engine="whether an analytical columnar engine is available"))
emit_kind('object-store', dict(versioning=T('bool'), public_read=T('bool')),
          dict(versioning="whether every overwrite keeps the previous version", public_read="whether anonymous reads are allowed"))
emit_kind('container-service', dict(min_instances=T('nat'), max_concurrency=T('nat')),
          dict(min_instances="instances kept warm at all times", max_concurrency="requests one instance serves at once"))

# ----------------------------------------------------------------------------- 3. operation stand-ins (fn_ records)
OPS = {}
def emit_op(name, params, effects, api):
    """A stand-in for the OpenAPI adapter's output for one Google Cloud API operation. Illustrative: in
    phase 4 the real adapter records replace these; the recipes change only in the addresses they cite.
    A parameter marked `:secret` is the proposed builtin (proposal 05); the spec schema does not know it
    yet, so the stand-in types it as string and the recipe's `secret` binding marks the intent."""
    pnames = [p.split(':')[0] for p in params]
    rec = {"schema_version": "0.2.0", "name_hints": [name],
           "param_docs": [f"{p} (stand-in)" for p in pnames],
           "signature": {"type": {"kind": "fn", "params": [T('string') for _ in params], "result": {"kind": "apply", "ctor": T('Maybe'), "args": [T('string')]}},
                         "refinements": [], "effects": effects, "capabilities": [], "terminates": "always"},
           "examples": [{"args": [S(f"<{p}>") for p in pnames], "result": {"kind": "variant", "tag": "Just", "payload": S("{}")}}],
           "intent_tags": ["io", "io/network/http"], "derived_from": None, "supersedes": None,
           "body_hash": f"expr_{blake3.blake3(canon({'kind': 'var', 'name': 'stand_in_' + name})).hexdigest()}"}
    OPS[name] = emit(f"op-{name}", 'fn', rec, 'function-record.v0.2.schema.json', what=f"operation stand-in: {api}")
W, R = ["net.write"], ["net.read"]
for spec in [
    ('compute_networks_insert', ['project', 'body'], W, 'compute.networks.insert'),
    ('compute_networks_get', ['project', 'network'], R, 'compute.networks.get'),
    ('compute_networks_delete', ['project', 'network'], W, 'compute.networks.delete'),
    ('compute_global_addresses_insert', ['project', 'body'], W, 'compute.globalAddresses.insert'),
    ('compute_global_addresses_get', ['project', 'address'], R, 'compute.globalAddresses.get'),
    ('compute_global_addresses_delete', ['project', 'address'], W, 'compute.globalAddresses.delete'),
    ('servicenetworking_connections_create', ['parent', 'body'], W, 'servicenetworking.services.connections.create'),
    ('servicenetworking_connections_list', ['parent', 'network'], R, 'servicenetworking.services.connections.list'),
    ('servicenetworking_connections_delete', ['parent', 'network'], W, 'servicenetworking.services.connections.deleteConnection'),
    ('sqladmin_instances_insert', ['project', 'body'], W, 'sqladmin.instances.insert'),
    ('sqladmin_instances_get', ['project', 'instance'], R, 'sqladmin.instances.get'),
    ('sqladmin_instances_delete', ['project', 'instance'], W, 'sqladmin.instances.delete'),
    ('sqladmin_users_insert', ['project', 'instance', 'body'], W, 'sqladmin.users.insert'),
    ('secretmanager_secrets_create', ['project', 'secret_id', 'body'], W, 'secretmanager.projects.secrets.create'),
    ('secretmanager_secrets_get', ['project', 'secret_id'], R, 'secretmanager.projects.secrets.get'),
    ('secretmanager_secrets_delete', ['project', 'secret_id'], W, 'secretmanager.projects.secrets.delete'),
    ('secretmanager_secrets_add_version', ['project', 'secret_id', 'payload:secret'], W, 'secretmanager.projects.secrets.addVersion'),
    ('alloydb_clusters_create', ['project', 'region', 'cluster_id', 'body'], W, 'alloydb.projects.locations.clusters.create'),
    ('alloydb_clusters_get', ['project', 'region', 'cluster_id'], R, 'alloydb.projects.locations.clusters.get'),
    ('alloydb_clusters_delete', ['project', 'region', 'cluster_id'], W, 'alloydb.projects.locations.clusters.delete'),
    ('alloydb_instances_create', ['project', 'region', 'cluster_id', 'instance_id', 'body'], W, 'alloydb.projects.locations.clusters.instances.create'),
    ('alloydb_instances_get', ['project', 'region', 'cluster_id', 'instance_id'], R, 'alloydb.projects.locations.clusters.instances.get'),
    ('alloydb_instances_delete', ['project', 'region', 'cluster_id', 'instance_id'], W, 'alloydb.projects.locations.clusters.instances.delete'),
    ('storage_buckets_insert', ['project', 'body'], W, 'storage.buckets.insert'),
    ('storage_buckets_get', ['bucket'], R, 'storage.buckets.get'),
    ('storage_buckets_delete', ['bucket'], W, 'storage.buckets.delete'),
    ('run_services_create', ['project', 'region', 'service_id', 'body'], W, 'run.projects.locations.services.create'),
    ('run_services_get', ['project', 'region', 'service_id'], R, 'run.projects.locations.services.get'),
    ('run_services_delete', ['project', 'region', 'service_id'], W, 'run.projects.locations.services.delete'),
]:
    emit_op(*spec)

# ----------------------------------------------------------------------------- 4. dishes and recipes
DISHES, DISH_DEFS, PUBLICATION_ORDER = {}, {}, []

def step(id_, op, args, refinements, outputs=None):
    s = {"id": id_, "target": OPS[op], "args": args, "refinements": refinements}
    if outputs: s["outputs"] = [{"name": n, "path": p} for n, p in outputs]
    return s

def readiness(cls, op, args, path, equals, limit):
    return {"class": cls, "probe": OPS[op], "args": args, "ready_when": {"path": path, "equals": equals}, "limit_seconds": limit}

def emit_dish(name, kind, provides, names, placements, wires, outputs, recipe, hints):
    rcp = {"kind": "recipe", "schema_version": "0.1.0", **recipe}
    rcp_addr = emit(f"recipe-{name}", 'rcp', rcp, 'recipe.schema.json', what=f"recipe of {name}")
    d = {"kind": "dish", "schema_version": "0.1.0", "name_hints": hints, "of_kind": KINDS[kind], "provides": provides,
         "inputs": {"names": [{"name": n} for n in names], "placements": placements, "wires": [{"name": n, "type": t} for n, t in wires]},
         "outputs": [{"name": n, "type": t} for n, t in outputs], "recipe": rcp_addr, "supersedes": None}
    validate(provides, 'value-expression.schema.json')
    addr = emit(f"dish-{name}", 'dish', d, 'dish.schema.json', what=f"dish {name} ({kind})")
    DISHES[name] = addr; PUBLICATION_ORDER.append(name)
    DISH_DEFS[name] = dict(kind=kind, provides=provides, names=names, placements=placements, wires=dict(wires), outputs=dict(outputs), recipe=rcp, recipe_addr=rcp_addr)
    return addr

PSA_PARENT = b_lit(S("services/servicenetworking.googleapis.com"))

emit_dish('vpc-psa', 'network',
    provides=REC(private_service_access=B(True), subnets=N(0), nat=B(False)),
    names=['name'], placements=[{"name": "project", "placement": "project"}], wires=[],
    outputs=[('network', TREF('network-ref'))],
    recipe=dict(
        components=[],
        operations=[
            step('net-insert', 'compute_networks_insert', [b_in('project'), b_json(name=b_in('name'), autoCreateSubnetworks=b_lit(B(False)))],
                 [ENS('network', [k_body(1, 'name')])], outputs=[('self-link', ['selfLink'])]),
            step('addr-insert', 'compute_global_addresses_insert',
                 [b_in('project'), b_json(name=b_der('name', '-psa'), purpose=b_lit(S("VPC_PEERING")), addressType=b_lit(S("INTERNAL")), prefixLength=b_lit(N(16)), network=b_oout('net-insert', 'self-link'))],
                 [REQ('network', [k_in('name')]), ENS('global-address', [k_body(1, 'name')])]),
            step('psa-connect', 'servicenetworking_connections_create',
                 [PSA_PARENT, b_json(network=b_oout('net-insert', 'self-link'), reservedPeeringRange=b_der('name', '-psa'))],
                 [REQ('network', [k_in('name')]), REQ('global-address', [k_der('name', '-psa')]), ENS('psa-connection', [k_in('name')])]),
        ],
        retire=[
            step('psa-delete', 'servicenetworking_connections_delete', [PSA_PARENT, b_in('name')], [ENS('psa-connection', [k_in('name')], 'absent')]),
            step('addr-delete', 'compute_global_addresses_delete', [b_in('project'), b_der('name', '-psa')], [REQ('psa-connection', [k_in('name')], 'absent'), ENS('global-address', [k_der('name', '-psa')], 'absent')]),
            step('net-delete', 'compute_networks_delete', [b_in('project'), b_in('name')], [REQ('psa-connection', [k_in('name')], 'absent'), REQ('global-address', [k_der('name', '-psa')], 'absent'), ENS('network', [k_in('name')], 'absent')]),
        ],
        readiness=[
            readiness('network', 'compute_networks_get', [b_in('project'), b_rkey(0)], ['kind'], S("compute#network"), 300),
            readiness('global-address', 'compute_global_addresses_get', [b_in('project'), b_rkey(0)], ['status'], S("RESERVED"), 300),
            readiness('psa-connection', 'servicenetworking_connections_list', [PSA_PARENT, b_rkey(0)], ['connections', 0, 'network'], S("*"), 600),
        ],
        create_only=['private_service_access'],
        outputs=[{"name": "network", "from": b_rec(name=b_in('name'), self_link=b_oout('net-insert', 'self-link'))}]),
    hints=['vpc_psa', 'network_private_service_access'])

emit_dish('cloudsql-pg15-ha-2r', 'relational-database',
    provides=REC(engine=V('Postgres'), major=N(15), ha=B(True), regions=N(2), encryption_at_rest=B(True), columnar_engine=B(False)),
    names=['name'], placements=[{"name": "project", "placement": "project"}, {"name": "regions", "placement": "regions", "count": 2}],
    wires=[('network', TREF('network-ref'))],
    outputs=[('endpoint', TREF('connection-endpoint')), ('credential', TREF('secret-reference'))],
    recipe=dict(
        components=[],
        operations=[
            step('inst-insert', 'sqladmin_instances_insert',
                 [b_in('project'), b_json(name=b_in('name'), region=b_in_at('regions', 0), databaseVersion=b_lit(S("POSTGRES_15")),
                                          settings=b_json(tier=b_lit(S("db-custom-2-7680")), availabilityType=b_lit(S("REGIONAL")),
                                                          ipConfiguration=b_json(ipv4Enabled=b_lit(B(False)), privateNetwork=b_wire('network', 'self_link'))))],
                 [REQ('psa-connection', [k_wire('network', 'name')]), ENS('sql-instance', [k_body(1, 'name')])],
                 outputs=[('ip', ['ipAddresses', 0, 'ipAddress'])]),
            step('replica-insert', 'sqladmin_instances_insert',
                 [b_in('project'), b_json(name=b_der('name', '-replica'), region=b_in_at('regions', 1), masterInstanceName=b_in('name'), databaseVersion=b_lit(S("POSTGRES_15")),
                                          settings=b_json(tier=b_lit(S("db-custom-2-7680"))))],
                 [REQ('sql-instance', [k_in('name')], 'ready'), ENS('sql-instance', [k_body(1, 'name')])]),
            step('secret-create', 'secretmanager_secrets_create', [b_in('project'), b_der('name', '-cred'), b_json(replication=b_json(automatic=b_json()))],
                 [ENS('secret', [k_der('name', '-cred')])]),
            step('secret-version', 'secretmanager_secrets_add_version', [b_in('project'), b_der('name', '-cred'), b_secret(32)],
                 [REQ('secret', [k_der('name', '-cred')]), ENS('secret-version', [k_der('name', '-cred')])], outputs=[('version', ['name'])]),
            step('user-insert', 'sqladmin_users_insert', [b_in('project'), b_in('name'), b_json(name=b_lit(S("app")), password=b_secret(32))],
                 [REQ('sql-instance', [k_in('name')], 'ready'), REQ('secret-version', [k_der('name', '-cred')]), ENS('sql-user', [k_in('name'), k_lit(S("app"))])]),
        ],
        retire=[
            step('replica-delete', 'sqladmin_instances_delete', [b_in('project'), b_der('name', '-replica')], [ENS('sql-instance', [k_der('name', '-replica')], 'absent')]),
            step('inst-delete', 'sqladmin_instances_delete', [b_in('project'), b_in('name')], [REQ('sql-instance', [k_der('name', '-replica')], 'absent'), ENS('sql-instance', [k_in('name')], 'absent'), ENS('sql-user', [k_in('name'), k_lit(S("app"))], 'absent')]),
            step('secret-delete', 'secretmanager_secrets_delete', [b_in('project'), b_der('name', '-cred')], [ENS('secret', [k_der('name', '-cred')], 'absent'), ENS('secret-version', [k_der('name', '-cred')], 'absent')]),
        ],
        readiness=[
            readiness('sql-instance', 'sqladmin_instances_get', [b_in('project'), b_rkey(0)], ['state'], S("RUNNABLE"), 900),
            readiness('secret', 'secretmanager_secrets_get', [b_in('project'), b_rkey(0)], ['name'], S("*"), 120),
        ],
        create_only=['engine', 'major', 'regions', 'encryption_at_rest'],
        outputs=[{"name": "endpoint", "from": b_rec(instance=b_in('name'), host=b_oout('inst-insert', 'ip'), port=b_lit(N(5432)))},
                 {"name": "credential", "from": b_rec(store=b_lit(S("secretmanager")), name=b_der('name', '-cred'), version=b_oout('secret-version', 'version'))}]),
    hints=['cloudsql_pg15_ha_2r', 'relational_database_postgres_15'])

emit_dish('alloydb-pg15-ha-2r', 'relational-database',
    provides=REC(engine=V('Postgres'), major=N(15), ha=B(True), regions=N(2), encryption_at_rest=B(True), columnar_engine=B(True)),
    names=['name'], placements=[{"name": "project", "placement": "project"}, {"name": "regions", "placement": "regions", "count": 2}],
    wires=[('network', TREF('network-ref'))],
    outputs=[('endpoint', TREF('connection-endpoint')), ('credential', TREF('secret-reference'))],
    recipe=dict(
        components=[],
        operations=[
            step('secret-create', 'secretmanager_secrets_create', [b_in('project'), b_der('name', '-cred'), b_json(replication=b_json(automatic=b_json()))], [ENS('secret', [k_der('name', '-cred')])]),
            step('secret-version', 'secretmanager_secrets_add_version', [b_in('project'), b_der('name', '-cred'), b_secret(32)],
                 [REQ('secret', [k_der('name', '-cred')]), ENS('secret-version', [k_der('name', '-cred')])], outputs=[('version', ['name'])]),
            step('cluster-create', 'alloydb_clusters_create',
                 [b_in('project'), b_in_at('regions', 0), b_in('name'), b_json(network=b_wire('network', 'self_link'), initialUser=b_json(user=b_lit(S("app")), password=b_secret(32)), continuousBackupConfig=b_json(enabled=b_lit(B(True))))],
                 [REQ('psa-connection', [k_wire('network', 'name')]), REQ('secret-version', [k_der('name', '-cred')]), ENS('alloydb-cluster', [k_in('name')])]),
            step('primary-create', 'alloydb_instances_create',
                 [b_in('project'), b_in_at('regions', 0), b_in('name'), b_der('name', '-primary'), b_json(instanceType=b_lit(S("PRIMARY")), availabilityType=b_lit(S("REGIONAL")))],
                 [REQ('alloydb-cluster', [k_in('name')], 'ready'), ENS('alloydb-instance', [k_der('name', '-primary')])], outputs=[('ip', ['ipAddress'])]),
            step('secondary-create', 'alloydb_clusters_create',
                 [b_in('project'), b_in_at('regions', 1), b_der('name', '-secondary'), b_json(secondaryConfig=b_json(primaryClusterName=b_in('name')), network=b_wire('network', 'self_link'))],
                 [REQ('alloydb-instance', [k_der('name', '-primary')], 'ready'), ENS('alloydb-cluster', [k_der('name', '-secondary')])]),
        ],
        retire=[
            step('secondary-delete', 'alloydb_clusters_delete', [b_in('project'), b_in_at('regions', 1), b_der('name', '-secondary')], [ENS('alloydb-cluster', [k_der('name', '-secondary')], 'absent')]),
            step('primary-delete', 'alloydb_instances_delete', [b_in('project'), b_in_at('regions', 0), b_in('name'), b_der('name', '-primary')], [REQ('alloydb-cluster', [k_der('name', '-secondary')], 'absent'), ENS('alloydb-instance', [k_der('name', '-primary')], 'absent')]),
            step('cluster-delete', 'alloydb_clusters_delete', [b_in('project'), b_in_at('regions', 0), b_in('name')], [REQ('alloydb-instance', [k_der('name', '-primary')], 'absent'), ENS('alloydb-cluster', [k_in('name')], 'absent')]),
            step('secret-delete', 'secretmanager_secrets_delete', [b_in('project'), b_der('name', '-cred')], [ENS('secret', [k_der('name', '-cred')], 'absent'), ENS('secret-version', [k_der('name', '-cred')], 'absent')]),
        ],
        readiness=[
            readiness('alloydb-cluster', 'alloydb_clusters_get', [b_in('project'), b_in_at('regions', 0), b_rkey(0)], ['state'], S("READY"), 1200),
            readiness('alloydb-instance', 'alloydb_instances_get', [b_in('project'), b_in_at('regions', 0), b_in('name'), b_rkey(0)], ['state'], S("READY"), 1200),
            readiness('secret', 'secretmanager_secrets_get', [b_in('project'), b_rkey(0)], ['name'], S("*"), 120),
        ],
        create_only=['engine', 'major', 'regions', 'encryption_at_rest', 'columnar_engine'],
        outputs=[{"name": "endpoint", "from": b_rec(instance=b_in('name'), host=b_oout('primary-create', 'ip'), port=b_lit(N(5432)))},
                 {"name": "credential", "from": b_rec(store=b_lit(S("secretmanager")), name=b_der('name', '-cred'), version=b_oout('secret-version', 'version'))}]),
    hints=['alloydb_pg15_ha_2r', 'relational_database_postgres_15_columnar'])

emit_dish('gcs-versioned-private', 'object-store',
    provides=REC(versioning=B(True), public_read=B(False)),
    names=['name'], placements=[{"name": "project", "placement": "project"}, {"name": "region", "placement": "region"}], wires=[],
    outputs=[('bucket', TREF('bucket-ref'))],
    recipe=dict(
        components=[],
        operations=[step('bucket-insert', 'storage_buckets_insert',
                         [b_in('project'), b_json(name=b_in('name'), location=b_in('region'), versioning=b_json(enabled=b_lit(B(True))),
                                                  iamConfiguration=b_json(uniformBucketLevelAccess=b_json(enabled=b_lit(B(True))), publicAccessPrevention=b_lit(S("enforced"))))],
                         [ENS('bucket', [k_body(1, 'name')])])],
        retire=[step('bucket-delete', 'storage_buckets_delete', [b_in('name')], [ENS('bucket', [k_in('name')], 'absent')])],
        readiness=[readiness('bucket', 'storage_buckets_get', [b_rkey(0)], ['kind'], S("storage#bucket"), 120)],
        create_only=['versioning'],
        outputs=[{"name": "bucket", "from": b_rec(name=b_in('name'))}]),
    hints=['gcs_versioned_private', 'object_store_versioned'])

emit_dish('cloudrun-min1-c80', 'container-service',
    provides=REC(min_instances=N(1), max_concurrency=N(80)),
    names=['name'], placements=[{"name": "project", "placement": "project"}, {"name": "region", "placement": "region"}],
    wires=[('network', TREF('network-ref')), ('database', TREF('connection-endpoint')), ('credential', TREF('secret-reference')), ('assets', TREF('bucket-ref'))],
    outputs=[('url', TREF('service-url'))],
    recipe=dict(
        components=[],
        operations=[step('svc-create', 'run_services_create',
                         [b_in('project'), b_in('region'), b_in('name'),
                          b_json(template=b_json(
                              scaling=b_json(minInstanceCount=b_lit(N(1))), maxInstanceRequestConcurrency=b_lit(N(80)),
                              vpcAccess=b_json(networkInterfaces=b_jlist(b_json(network=b_wire('network', 'self_link'))), egress=b_lit(S("PRIVATE_RANGES_ONLY"))),
                              containers=b_jlist(b_json(image=b_lit(S("us-docker.pkg.dev/cloudrun/container/hello")), env=b_jlist(
                                  b_json(name=b_lit(S("DB_HOST")), value=b_wire('database', 'host')),
                                  b_json(name=b_lit(S("DB_PORT")), value=b_wire('database', 'port')),
                                  b_json(name=b_lit(S("DB_PASSWORD")), valueSource=b_json(secretKeyRef=b_json(secret=b_wire('credential', 'name'), version=b_wire('credential', 'version')))),
                                  b_json(name=b_lit(S("ASSETS_BUCKET")), value=b_wire('assets', 'name')))))))],
                         [REQ('network', [k_wire('network', 'name')]), REQ('sql-instance', [k_wire('database', 'instance')], 'ready'),
                          REQ('secret-version', [k_wire('credential', 'name')]), REQ('bucket', [k_wire('assets', 'name')]), ENS('run-service', [k_in('name')])],
                         outputs=[('uri', ['uri'])])],
        retire=[step('svc-delete', 'run_services_delete', [b_in('project'), b_in('region'), b_in('name')], [ENS('run-service', [k_in('name')], 'absent')])],
        readiness=[readiness('run-service', 'run_services_get', [b_in('project'), b_in('region'), b_rkey(0)], ['terminalCondition', 'state'], S("CONDITION_SUCCEEDED"), 600)],
        create_only=[],
        outputs=[{"name": "url", "from": b_rec(url=b_oout('svc-create', 'uri'))}]),
    hints=['cloudrun_min1_c80', 'container_service_warm'])

# ----------------------------------------------------------------------------- 5. needs list and profile
NEEDS = {"kind": "needs-list", "schema_version": "0.1.0", "name_hints": ["webshop_api_2026_11"], "needs": [
    {"name": "api-network", "of_kind": KINDS['network'], "constraints": app("eq", var("private_service_access"), lit(True)), "wires": []},
    {"name": "primary-db", "of_kind": KINDS['relational-database'],
     "constraints": AND(app("eq", var("engine"), lit(V('Postgres'))), app("ge", var("major"), lit(15)), app("eq", var("encryption_at_rest"), lit(True))),
     "wires": [{"role": "lives-in", "to": "api-network"}]},
    {"name": "assets", "of_kind": KINDS['object-store'], "constraints": AND(app("eq", var("versioning"), lit(True)), app("eq", var("public_read"), lit(False))), "wires": []},
    {"name": "api", "of_kind": KINDS['container-service'], "constraints": AND(app("ge", var("min_instances"), lit(1)), app("ge", var("max_concurrency"), lit(80))),
     "wires": [{"role": "lives-in", "to": "api-network"}, {"role": "reads-from", "to": "primary-db"}, {"role": "reads-from", "to": "assets"}]},
]}
for n in NEEDS['needs']: validate(n['constraints'], 'predicate-expression.schema.json')
NEEDS_ADDR = emit('needs-list-webshop', 'needs', NEEDS, 'needs-list.schema.json', what='the needs list (glossary worked example)')

PROFILE = {"kind": "profile", "schema_version": "0.1.0", "name": "production",
           "constraints": [{"of_kind": KINDS['relational-database'], "predicate": AND(app("eq", var("ha"), lit(True)), app("ge", var("regions"), lit(2)))},
                           {"of_kind": KINDS['container-service'], "predicate": app("ge", var("min_instances"), lit(1))}],
           "sharing": []}
PROFILE_ADDR = emit('profile-production', 'prof', PROFILE, 'profile.schema.json', what='the production profile')

# ----------------------------------------------------------------------------- a tiny predicate evaluator (checks the hand trace; not the mechanism)
def value_of(v):
    k = v['kind']
    if k in ('bool', 'nat', 'int', 'string', 'float'): return v['value']
    if k == 'variant': return ('variant', v['tag'])
    raise ValueError(k)
def holds(pred, provides):
    env = {f['name']: value_of(f['value']) for f in provides['fields']}
    def ev(p):
        k = p['kind']
        if k == 'var': return env[p['name']]
        if k == 'lit':
            v = p['value']
            return ('variant', v['tag']) if isinstance(v, dict) and v.get('kind') == 'variant' else v
        if k == 'app':
            a = [ev(x) for x in p['args']]
            return {'and': lambda: all(a), 'or': lambda: any(a), 'not': lambda: not a[0], 'eq': lambda: a[0] == a[1], 'neq': lambda: a[0] != a[1],
                    'ge': lambda: a[0] >= a[1], 'gt': lambda: a[0] > a[1], 'le': lambda: a[0] <= a[1], 'lt': lambda: a[0] < a[1]}[p['op']]()
        raise ValueError(k)
    return bool(ev(pred))

# ----------------------------------------------------------------------------- 6. the selector's result: the plate (hand-composed, then checked)
def menu_candidates(need):
    out = []
    prof = {c['of_kind']: c['predicate'] for c in PROFILE['constraints']}
    for name in PUBLICATION_ORDER:
        d = DISH_DEFS[name]
        if KINDS[d['kind']] != need['of_kind']: continue
        if holds(need['constraints'], d['provides']) and (need['of_kind'] not in prof or holds(prof[need['of_kind']], d['provides'])):
            out.append(name)
    return out
CANDIDATES = {n['name']: menu_candidates(n) for n in NEEDS['needs']}
assert CANDIDATES == {'api-network': ['vpc-psa'], 'primary-db': ['cloudsql-pg15-ha-2r', 'alloydb-pg15-ha-2r'], 'assets': ['gcs-versioned-private'], 'api': ['cloudrun-min1-c80']}, CANDIDATES

SERVINGS = [("s1", 'vpc-psa'), ("s2", 'cloudsql-pg15-ha-2r'), ("s3", 'gcs-versioned-private'), ("s4", 'cloudrun-min1-c80')]
COVERAGE = [("api-network", "s1"), ("primary-db", "s2"), ("assets", "s3"), ("api", "s4")]
WIRES = [
    ("primary-db", "lives-in", "api-network", ("s1", "network"), ("s2", "network")),
    ("api", "lives-in", "api-network", ("s1", "network"), ("s4", "network")),
    ("api", "reads-from", "primary-db", ("s2", "endpoint"), ("s4", "database")),
    ("api", "reads-from", "primary-db", ("s2", "credential"), ("s4", "credential")),
    ("api", "reads-from", "assets", ("s3", "bucket"), ("s4", "assets")),
]
def plate_record(needs_addr, profile_addr, servings, coverage, wires):
    return {"kind": "plate", "schema_version": "0.1.0", "needs_list": needs_addr, "profile": profile_addr,
            "servings": [{"id": s, "dish": DISHES[d]} for s, d in servings],
            "coverage": [{"need": n, "serving": s} for n, s in coverage],
            "wires": [{"need": n, "role": r, "to": t, "output": {"serving": o[0], "port": o[1]}, "input": {"serving": i[0], "port": i[1]}} for n, r, t, o, i in wires],
            "overrides": [], "cleanup_manifest": []}
PLATE = plate_record(NEEDS_ADDR, PROFILE_ADDR, SERVINGS, COVERAGE, WIRES)
PLATE_ADDR = emit('plate-webshop-production', 'plate', PLATE, 'plate.schema.json', what='the plate: four servings, five port bindings')

def check_coverage(plate, needs, profile, dish_of):
    """The coverage check of algorithms.md section 2, over the records. Raises on any failure."""
    from collections import Counter
    byneed = {c['need']: c['serving'] for c in plate['coverage']}
    assert sorted(byneed) == sorted(n['name'] for n in needs['needs']) and len(plate['coverage']) == len(needs['needs']), 'every need once'
    prof = {c['of_kind']: c['predicate'] for c in profile['constraints']}
    share = {s['of_kind']: s['allowed'] for s in profile['sharing']}
    for n in needs['needs']:
        d = dish_of(byneed[n['name']])
        assert d['of_kind'] == n['of_kind'], f"kind-mismatch {n['name']}"
        assert holds(n['constraints'], d['provides']), f"need-not-satisfied {n['name']}"
        if n['of_kind'] in prof: assert holds(prof[n['of_kind']], d['provides']), f"profile-not-satisfied {n['name']}"
    for s, c in Counter(byneed.values()).items():
        if c > 1: assert share.get(dish_of(s)['of_kind']), f"sharing-not-permitted {s}"
    fed = Counter()
    for n in needs['needs']:
        for w in n['wires']:
            ws = [x for x in plate['wires'] if x['need'] == n['name'] and x['role'] == w['role'] and x['to'] == w['to']]
            assert ws, f"wire-unbound {n['name']} {w['role']}"
            for x in ws:
                assert x['input']['serving'] == byneed[n['name']] and x['output']['serving'] == byneed[w['to']], 'wire-misrouted'
                di, do = dish_of(x['input']['serving']), dish_of(x['output']['serving'])
                ti = next(p['type'] for p in di['inputs']['wires'] if p['name'] == x['input']['port'])
                to = next(p['type'] for p in do['outputs'] if p['name'] == x['output']['port'])
                assert canon(ti) == canon(to), f"wire-mistyped {n['name']} {w['role']}"
                fed[(x['input']['serving'], x['input']['port'])] += 1
    for s in plate['servings']:
        for p in dish_of(s['id'])['inputs']['wires']:
            assert fed[(s['id'], p['name'])] == 1, f"input-unfed-or-twice {s['id']} {p['name']}"
    return True
dish_by_serving = {s: RECORDS[DISHES[d]][1] for s, d in SERVINGS}
check_coverage(PLATE, NEEDS, PROFILE, lambda s: dish_by_serving[s])

# ----------------------------------------------------------------------------- 7. the instance generator (the organization's naming rule, applied)
LINEAGE, GENERATION = "acme/webshop/production", 7
def naming_rule(lineage, g, need, input_name):
    return f"{lineage.split('/')[1]}-g{g}-{need}"
def placement_rule(kind, g):
    base = {"project": "acme-webshop-prod"}
    if kind == 'relational-database': base["regions"] = ["europe-west1", "europe-west4"]
    elif kind != 'network': base["region"] = "europe-west1"
    return base
def derived_names(recipe, top_names):
    out = {}
    def walk(x):
        if isinstance(x, dict):
            if x.get('kind') == 'derive' and 'from' in x and 'suffix' in x:
                out[(x['from'], x['suffix'])] = top_names[x['from']] + x['suffix']
            for v in x.values(): walk(v)
        elif isinstance(x, list):
            for v in x: walk(v)
    walk(recipe)
    return out
def instance_record(plate_addr, lineage, g, servings, coverage, project=None, mark=''):
    need_of = {s: n for n, s in coverage}
    out = []
    for sid, dname in servings:
        d = DISH_DEFS[dname]
        top = {n: mark + naming_rule(lineage, g, need_of[sid], n) for n in d['names']}
        names = [{"path": [n], "value": v} for n, v in top.items()]
        for (frm, suf), v in sorted(derived_names(d['recipe'], top).items()):
            names.append({"path": [frm], "suffix": suf, "value": v})
        pl = placement_rule(d['kind'], g)
        if project: pl['project'] = project
        out.append({"id": sid, "placement": pl, "names": names})
    return {"kind": "instance", "schema_version": "0.1.0", "plate": plate_addr, "lineage": lineage, "generation": g, "servings": out}
INSTANCE = instance_record(PLATE_ADDR, LINEAGE, GENERATION, SERVINGS, COVERAGE)
INSTANCE_ADDR = emit('instance-webshop-g7', 'inst', INSTANCE, 'instance.schema.json', what='the instance: every literal name and placement, generation 7')

# ----------------------------------------------------------------------------- 8. expansion and the planner (algorithms.md sections 5.5 and 6)
LATER_RE = re.compile(r"\{\{([a-z0-9-]+/[a-z0-9-]+)\.")

def expand(plate, instance, servings_defs, wires):
    """Flatten servings into steps with plan-time bindings grounded. No components in this example (every
    dish is operations only), so expansion is one level; the component case substitutes `bind` and recurses."""
    inst = {s['id']: s for s in instance['servings']}
    out_src = {(sid, o['name']): o['from'] for sid, dname in servings_defs for o in DISH_DEFS[dname]['recipe']['outputs']}
    wire_src = {i: o for n, r, t, o, i in wires}
    def names_of(sid):
        top = {e['path'][0]: e['value'] for e in inst[sid]['names'] if 'suffix' not in e}
        der = {(e['path'][0], e['suffix']): e['value'] for e in inst[sid]['names'] if 'suffix' in e}
        return top, der
    def ground_binding(sid, b):
        top, der = names_of(sid); pl = inst[sid]['placement']; k = b['kind']
        if k == 'lit': return b['value']
        if k == 'input':
            if b['name'] in top: return S(top[b['name']])
            v = pl[b['name']]; return S(v) if isinstance(v, str) else {"kind": "list", "items": [S(x) for x in v]}
        if k == 'input-at': return S(pl[b['name']][b['index']])
        if k == 'derive': return S(der[(b['from'], b['suffix'])])
        if k == 'wire':
            src = wire_src[(sid, b['name'])]
            return ground_binding(src[0], out_src[src]['fields'][b['field']])
        return None
    def later_ref(sid, b):
        if b['kind'] == 'operation-output': return f"{sid}/{b['operation']}.{b['output']}"
        src = wire_src[(sid, b['name'])]; fb = out_src[src]['fields'][b['field']]
        return f"{src[0]}/{fb['operation']}.{fb['output']}"
    def render_json(sid, b):
        k = b['kind']
        if k == 'json': return {f: render_json(sid, v) for f, v in b['fields'].items()}
        if k == 'json-list': return [render_json(sid, v) for v in b['items']]
        if k == 'secret': return "{{secret}}"
        g = ground_binding(sid, b)
        if g is not None: return g['value'] if g['kind'] != 'list' else [x['value'] for x in g['items']]
        return "{{" + later_ref(sid, b) + "}}"
    def ground_arg(sid, b):
        k = b['kind']
        if k == 'json':
            txt = json.dumps(render_json(sid, b), separators=(',', ':'))
            return {"kind": "variant", "tag": "LaterJson", "payload": S(txt)} if '{{' in txt else S(txt)
        if k == 'secret': return {"kind": "variant", "tag": "Secret"}
        g = ground_binding(sid, b)
        return g if g is not None else {"kind": "variant", "tag": "Later", "payload": S(later_ref(sid, b))}
    def ground_key(sid, step_args, part):
        k = part['kind']
        if k == 'lit': return part['value']
        if k in ('input', 'derive', 'wire'):
            g = ground_binding(sid, part)
            if g is None: raise ValueError(f"key-not-literal {sid} {part}")
            return g
        if k == 'body-field':
            body = step_args[part['arg']]
            tmpl = json.loads(body['value'] if body['kind'] == 'string' else body['payload']['value'])
            v = tmpl[part['field']]
            if isinstance(v, str) and v.startswith('{{'): raise ValueError(f"key-not-literal {sid} body-field {part['field']}")
            return S(v)
        if k == 'arg':
            v = step_args[part['arg']]
            if v['kind'] == 'variant': raise ValueError('key-not-literal arg')
            return v
        raise ValueError(k)
    steps, data_edges = [], []
    for sid, dname in servings_defs:
        for st in DISH_DEFS[dname]['recipe']['operations']:
            label = f"{sid}/{st['id']}"
            args = [ground_arg(sid, a) for a in st['args']]
            refs = [{"kind": r['kind'], "resource": {"class": r['resource']['class'], "key": [ground_key(sid, args, p) for p in r['resource']['key']]}, "state": r['state']} for r in st['refinements']]
            steps.append({"label": label, "serving": sid, "step": st['id'], "target": st['target'], "args": args, "refinements": refs, "dish": dname})
            blob = json.dumps(args)
            for m in set(LATER_RE.findall(blob) + re.findall(r'"Later", "payload": \{"kind": "string", "value": "([a-z0-9-]+/[a-z0-9-]+)\.', blob)):
                data_edges.append((m, label))
    return steps, data_edges

def readiness_for(cls):
    for d in DISH_DEFS.values():
        for r in d['recipe']['readiness']:
            if r['class'] == cls: return r
    return None

def plan(steps, data_edges, instance):
    """Algorithms.md section 6. Returns (plan record without hash, waves of labels)."""
    inst = {s['id']: s for s in instance['servings']}
    bylabel = {s['label']: s for s in steps}
    def gkey(res): return (res['class'], canon(res['key']))
    ensures = {}
    for s in steps:
        for r in s['refinements']:
            if r['kind'] == 'ensures': ensures.setdefault(gkey(r['resource']), []).append((s['label'], r['state']))
    edges, assume, waits = set(), [], {}
    def satisfies(have, want): return have == want or (want == 'exists' and have == 'ready')
    for s in steps:
        for r in s['refinements']:
            if r['kind'] != 'requires': continue
            g = gkey(r['resource']); want = r['state']
            cands = [(l, st) for l, st in ensures.get(g, []) if satisfies(st, want)]
            if len(cands) > 1: raise ValueError(f"ambiguous-ensures {r['resource']}")
            if cands: edges.add((cands[0][0], s['label'])); continue
            weaker = [(l, st) for l, st in ensures.get(g, []) if want == 'ready' and st == 'exists']
            if weaker:
                wl = f"wait/{r['resource']['class']}/{r['resource']['key'][0]['value']}"
                if wl not in waits:
                    rd = readiness_for(r['resource']['class'])
                    if rd is None: raise ValueError(f"no-readiness-declaration {r['resource']['class']}")
                    sid = weaker[0][0].split('/')[0]; pl = inst[sid]['placement']
                    top = {e['path'][0]: e['value'] for e in inst[sid]['names'] if 'suffix' not in e}
                    def parg(b):
                        if b['kind'] == 'resource-key': return r['resource']['key'][b['index']]
                        if b['kind'] == 'input': return S(top[b['name']]) if b['name'] in top else S(pl[b['name']])
                        if b['kind'] == 'input-at': return S(pl[b['name']][b['index']])
                        if b['kind'] == 'lit': return b['value']
                        raise ValueError(b)
                    waits[wl] = {"kind": "wait", "label": wl, "resource": r['resource'], "until": "ready",
                                 "probe": {"target": rd['probe'], "args": [parg(b) for b in rd['args']]},
                                 "ready_when": rd['ready_when'], "interval_seconds": 15, "limit_seconds": rd['limit_seconds']}
                    edges.add((weaker[0][0], wl))
                    ensures.setdefault(g, []).append((wl, 'ready'))
                edges.add((wl, s['label'])); continue
            assume.append({"resource": r['resource'], "state": want})
    for a, b in data_edges: edges.add((a, b))
    nodes = [s['label'] for s in steps] + list(waits)
    preds = {n: {a for a, b in edges if b == n} for n in nodes}
    waves, placed = [], set()
    while len(placed) < len(nodes):
        wave = sorted(n for n in nodes if n not in placed and preds[n] <= placed)
        if not wave: raise ValueError('dependency-cycle')
        waves.append(wave); placed |= set(wave)
    flat = [n for w in waves for n in w]
    idx = {n: i for i, n in enumerate(flat)}
    def to_step(n):
        if n in waits: return dict(waits[n])
        s = bylabel[n]; return {"target": s['target'], "args": s['args'], "label": n}
    rec = {"kind": "plan", "schema_version": "0.1.0", "assume": assume, "steps": [to_step(n) for n in flat], "waves": [[idx[n] for n in w] for w in waves]}
    return rec, waves

STEPS, DATA_EDGES = expand(PLATE, INSTANCE, SERVINGS, WIRES)
PLAN, WAVES = plan(STEPS, DATA_EDGES, INSTANCE)
assert PLAN['assume'] == [], PLAN['assume']
PLAN_ADDR = emit('plan-webshop-g7', 'plan', PLAN, 'plan-extension.schema.json', what=f"the plan: {len(PLAN['steps'])} steps in {len(WAVES)} waves")

# ----------------------------------------------------------------------------- 9. the receipt (what the kitchen would record)
def ensured_resources(steps_for_serving):
    return [{"class": r['resource']['class'], "key": r['resource']['key'], "state": r['state']}
            for s in steps_for_serving for r in s['refinements'] if r['kind'] == 'ensures']
def serving_outputs(sid, dname, instance, plausible):
    d = DISH_DEFS[dname]; inst = {s['id']: s for s in instance['servings']}[sid]
    top = {e['path'][0]: e['value'] for e in inst['names'] if 'suffix' not in e}
    der = {(e['path'][0], e['suffix']): e['value'] for e in inst['names'] if 'suffix' in e}
    def val(b):
        k = b['kind']
        if k == 'lit': return b['value']
        if k == 'input': return S(top[b['name']]) if b['name'] in top else S(inst['placement'][b['name']])
        if k == 'derive': return S(der[(b['from'], b['suffix'])])
        if k == 'operation-output': return plausible.get((sid, b['operation'], b['output']), S(f"<observed {b['operation']}.{b['output']}>"))
        if k == 'record': return REC(**{f: val(v) for f, v in b['fields'].items()})
        raise ValueError(b)
    return [{"port": o['name'], "value": val(o['from'])} for o in d['recipe']['outputs']]
PLAUSIBLE = {("s1", "net-insert", "self-link"): S("https://www.googleapis.com/compute/v1/projects/acme-webshop-prod/global/networks/webshop-g7-api-network"),
             ("s2", "inst-insert", "ip"): S("10.42.0.3"), ("s2", "secret-version", "version"): S("projects/acme-webshop-prod/secrets/webshop-g7-primary-db-cred/versions/1"),
             ("s4", "svc-create", "uri"): S("https://webshop-g7-api-abc123-ew.a.run.app")}
def receipt_record(kind, plate_addr, inst_addr, plan_addr, supersedes, servings_status, started, finished, lineage, g):
    return {"kind": "receipt", "schema_version": "0.1.0", "receipt_kind": kind, "plate": plate_addr, "instance": inst_addr, "lineage": lineage, "generation": g,
            "plan": plan_addr, "supersedes": supersedes, "servings": servings_status, "cleanup_manifest": [], "started": started, "finished": finished}
RECEIPT_SERVINGS = [{"id": sid, "dish": DISHES[dname], "recipe": DISH_DEFS[dname]['recipe_addr'], "status": "provisioned",
                     "outputs": serving_outputs(sid, dname, INSTANCE, PLAUSIBLE), "resources": ensured_resources([s for s in STEPS if s['serving'] == sid])}
                    for sid, dname in SERVINGS]
RECEIPT = receipt_record("provisioning", PLATE_ADDR, INSTANCE_ADDR, PLAN_ADDR, None, RECEIPT_SERVINGS, "2026-11-12T20:00:00Z", "2026-11-12T20:21:40Z", LINEAGE, GENERATION)
RECEIPT_ADDR = emit('receipt-webshop-g7', 'rcpt', RECEIPT, 'receipt.schema.json', what='the provisioning receipt of generation 7')

# ----------------------------------------------------------------------------- 10. scrub: plan over the retire steps, and the scrub receipt
def expand_retire(servings_defs, instance):
    inst = {s['id']: s for s in instance['servings']}
    steps = []
    for sid, dname in servings_defs:
        d = DISH_DEFS[dname]; top = {e['path'][0]: e['value'] for e in inst[sid]['names'] if 'suffix' not in e}
        der = {(e['path'][0], e['suffix']): e['value'] for e in inst[sid]['names'] if 'suffix' in e}; pl = inst[sid]['placement']
        def g(b):
            k = b['kind']
            if k == 'lit': return b['value']
            if k == 'input': return S(top[b['name']]) if b['name'] in top else S(pl[b['name']])
            if k == 'input-at': return S(pl[b['name']][b['index']])
            if k == 'derive': return S(der[(b['from'], b['suffix'])])
            raise ValueError(b)
        for st in d['recipe']['retire']:
            args = [g(a) for a in st['args']]
            refs = [{"kind": r['kind'], "resource": {"class": r['resource']['class'], "key": [g(p) if p['kind'] != 'lit' else p['value'] for p in r['resource']['key']]}, "state": r['state']} for r in st['refinements']]
            steps.append({"label": f"{sid}/{st['id']}", "serving": sid, "step": st['id'], "target": st['target'], "args": args, "refinements": refs, "dish": dname})
    return steps
def reverse_edges(prov_steps, retire_steps):
    """Cross-serving dependencies of the provisioning run, reversed: a serving that required another's resource is retired before it."""
    cross = set()
    for s in prov_steps:
        for r in s['refinements']:
            if r['kind'] == 'requires':
                for t in prov_steps:
                    if t['serving'] != s['serving'] and any(e['kind'] == 'ensures' and canon(e['resource']) == canon(r['resource']) for e in t['refinements']):
                        cross.add((s['serving'], t['serving']))
    edges = []
    for dependent, provider in cross:
        for a in [x['label'] for x in retire_steps if x['serving'] == dependent]:
            for b in [x['label'] for x in retire_steps if x['serving'] == provider]: edges.append((a, b))
    return edges
RETIRE_STEPS = expand_retire(SERVINGS, INSTANCE)
SCRUB_PLAN, SCRUB_WAVES = plan(RETIRE_STEPS, reverse_edges(STEPS, RETIRE_STEPS), INSTANCE)
SCRUB_PLAN_ADDR = emit('plan-webshop-g7-scrub', 'plan', SCRUB_PLAN, 'plan-extension.schema.json', what=f"the scrub plan: {len(SCRUB_PLAN['steps'])} steps in {len(SCRUB_WAVES)} waves")
SCRUB_SERVINGS = [{"id": sid, "dish": DISHES[dname], "recipe": DISH_DEFS[dname]['recipe_addr'], "status": "scrubbed",
                   "resources": [dict(r, state="absent") for r in ensured_resources([s for s in STEPS if s['serving'] == sid])]} for sid, dname in SERVINGS]
SCRUB_RECEIPT = receipt_record("scrub", PLATE_ADDR, INSTANCE_ADDR, SCRUB_PLAN_ADDR, RECEIPT_ADDR, SCRUB_SERVINGS, "2026-11-13T09:00:00Z", "2026-11-13T09:14:05Z", LINEAGE, GENERATION)
SCRUB_RECEIPT_ADDR = emit('receipt-webshop-g7-scrub', 'rcpt', SCRUB_RECEIPT, 'receipt.schema.json', what='the scrub receipt superseding the provisioning receipt')

# ----------------------------------------------------------------------------- 11. test provisioning per dish: harness plate, test instance, receipts, signed attestation
def harness_for(dname):
    """The chef's harness: the dish plus an offered dish for each of its wire inputs (the first by publication
    order whose output type fits), providers themselves wired the same way."""
    chosen = []  # dish names in dependency order
    def need_providers(dn):
        for n, t in DISH_DEFS[dn]['wires'].items():
            prov = next(c for c in PUBLICATION_ORDER if c != dn and any(canon(ot) == canon(t) for ot in DISH_DEFS[c]['outputs'].values()))
            if prov not in chosen:
                need_providers(prov); chosen.append(prov)
    need_providers(dname); chosen.append(dname)
    servings = [(f"s{i+1}", dn) for i, dn in enumerate(chosen)]
    sid_of = {dn: sid for sid, dn in servings}
    nid_of = {dn: (f"under-test-{DISH_DEFS[dn]['kind']}" if dn == dname else f"harness-{DISH_DEFS[dn]['kind']}") for dn in chosen}
    coverage = [(nid_of[dn], sid_of[dn]) for dn in chosen]
    wires = []
    for dn in chosen:
        for n, t in DISH_DEFS[dn]['wires'].items():
            prov = next(c for c in chosen if c != dn and any(canon(ot) == canon(t) for ot in DISH_DEFS[c]['outputs'].values()))
            oport = next(on for on, ot in DISH_DEFS[prov]['outputs'].items() if canon(ot) == canon(t))
            wires.append((nid_of[dn], "wired", nid_of[prov], (sid_of[prov], oport), (sid_of[dn], n)))
    needs = []
    for dn in chosen:
        seen, ws = set(), []
        for w in wires:
            if w[0] == nid_of[dn] and w[2] not in seen: seen.add(w[2]); ws.append({"role": "wired", "to": w[2]})
        needs.append({"name": nid_of[dn], "of_kind": KINDS[DISH_DEFS[dn]['kind']], "constraints": lit(True), "wires": ws})
    return servings, coverage, wires, needs

TEST_PROFILE = {"kind": "profile", "schema_version": "0.1.0", "name": "test-harness", "constraints": [], "sharing": []}
TEST_PROFILE_ADDR = emit('profile-test-harness', 'prof', TEST_PROFILE, 'profile.schema.json', what="the attester's harness profile: no constraints")
ATTESTATIONS, HARNESS_WAVES = {}, {}
for dname in PUBLICATION_ORDER:
    servings, coverage, wires, needs = harness_for(dname)
    hn = {"kind": "needs-list", "schema_version": "0.1.0", "name_hints": [f"harness_{dname.replace('-', '_')}"], "needs": needs}
    hn_addr = emit(f"harness-needs-{dname}", 'needs', hn, 'needs-list.schema.json', what=f"harness needs list for {dname}")
    hp = plate_record(hn_addr, TEST_PROFILE_ADDR, servings, coverage, wires)
    hp_addr = emit(f"harness-plate-{dname}", 'plate', hp, 'plate.schema.json', what=f"harness plate for {dname}")
    check_coverage(hp, hn, TEST_PROFILE, lambda s, servings=servings: RECORDS[DISHES[dict(servings)[s]]][1])
    lineage = f"test/{DISHES[dname][5:21]}"
    ti = instance_record(hp_addr, lineage, 1, servings, coverage, project='nl-attester-sandbox', mark='t-')
    ti_addr = emit(f"harness-instance-{dname}", 'inst', ti, 'instance.schema.json', what=f"test instance for {dname}")
    t_steps, t_edges = expand(hp, ti, servings, wires)
    t_plan, t_waves = plan(t_steps, t_edges, ti)
    assert t_plan['assume'] == []
    HARNESS_WAVES[dname] = t_waves
    t_plan_addr = emit(f"harness-plan-{dname}", 'plan', t_plan, 'plan-extension.schema.json', what=f"test plan for {dname} ({len(t_plan['steps'])} steps, {len(t_waves)} waves)")
    t_serv = [{"id": sid, "dish": DISHES[dn], "recipe": DISH_DEFS[dn]['recipe_addr'], "status": "provisioned", "outputs": serving_outputs(sid, dn, ti, {}),
               "resources": ensured_resources([s for s in t_steps if s['serving'] == sid])} for sid, dn in servings]
    t_rcpt = receipt_record("provisioning", hp_addr, ti_addr, t_plan_addr, None, t_serv, "2026-11-11T08:00:00Z", "2026-11-11T08:30:00Z", lineage, 1)
    t_rcpt_addr = emit(f"harness-receipt-{dname}", 'rcpt', t_rcpt, 'receipt.schema.json', what=f"test provisioning receipt for {dname}")
    t_scrub_serv = [{"id": s['id'], "dish": s['dish'], "recipe": s['recipe'], "status": "scrubbed", "resources": [dict(r, state="absent") for r in s['resources']]} for s in t_serv]
    t_scrub = receipt_record("scrub", hp_addr, ti_addr, t_plan_addr, t_rcpt_addr, t_scrub_serv, "2026-11-11T08:30:00Z", "2026-11-11T08:45:00Z", lineage, 1)
    t_scrub_addr = emit(f"harness-scrub-{dname}", 'rcpt', t_scrub, 'receipt.schema.json', what=f"test scrub receipt for {dname}")
    att = {"kind": "provisioning-attestation", "schema_version": "0.1.0", "dish": DISHES[dname], "recipe": DISH_DEFS[dname]['recipe_addr'],
           "harness_plate": hp_addr, "instance": ti_addr, "result": "pass", "receipt": t_rcpt_addr, "scrub_receipt": t_scrub_addr,
           "started": "2026-11-11T08:00:00Z", "finished": "2026-11-11T08:45:00Z", "from": ATTESTER}
    ATTESTATIONS[dname] = emit(f"attestation-{dname}", 'patt', att, 'provisioning-attestation.schema.json', signed=True, what=f"signed passing attestation for {dname}")

# a made-from statement, so that record kind is exercised: the needs list was copied from an October draft without the object store
EARLIER = {"kind": "needs-list", "schema_version": "0.1.0", "name_hints": ["webshop_api_2026_10_draft"],
           "needs": [dict(n, wires=[w for w in n['wires'] if w['to'] != 'assets']) for n in NEEDS['needs'] if n['name'] != 'assets']}
EARLIER_ADDR = emit('needs-list-webshop-draft', 'needs', EARLIER, 'needs-list.schema.json', what='the earlier draft needs list (no object store)')
STMT = {"kind": "statement", "schema_version": "0.1.0", "statement": {"kind": "made-from", "needs_list": NEEDS_ADDR, "from": EARLIER_ADDR, "note": "The October draft had no object store; the assets need was added when the release gained file uploads."},
        "timestamp": "2026-11-12T19:00:00Z", "from": ATTESTER}
emit('statement-made-from', 'stmt', STMT, 'statement.schema.json', signed=True, what='a made-from statement (the configurator wrote it)')

# ----------------------------------------------------------------------------- 12. cross-reference check, hash and signature verification
ADDR_RE = re.compile(r'\b(?:fn|expr|type|cert|wgt|evl|trc|plan|msg|kind|dish|rcp|needs|prof|plate|inst|rcpt|patt|stmt)_[0-9a-f]{64}\b')
unresolved = {(f, a) for h, (f, obj) in RECORDS.items() for a in set(ADDR_RE.findall(json.dumps(obj))) if a not in RECORDS and a != h and not a.startswith('expr_')}
assert not unresolved, unresolved
SIGNED_KINDS = ('provisioning-attestation', 'statement')
for h, (f, obj) in RECORDS.items():
    if 'hash' in obj: assert address(h.split('_')[0], obj, obj.get('kind') in SIGNED_KINDS) == h, f
pub = Ed25519PublicKey.from_public_bytes(bytes.fromhex(ATTESTER_PUB))
for h, (f, obj) in RECORDS.items():
    if obj.get('kind') in SIGNED_KINDS: pub.verify(base64.b64decode(obj['signature'].split(':')[1]), canon({k: v for k, v in obj.items() if k != 'signature'}))
for s in INSTANCE['servings']:
    top = {e['path'][0]: e['value'] for e in s['names'] if 'suffix' not in e}
    for e in s['names']:
        if 'suffix' in e: assert e['value'] == top[e['path'][0]] + e['suffix']

# ----------------------------------------------------------------------------- 13. addresses.md
lines = ["# Addresses of the worked example's records", "",
         "Generated by `build.py`. Every address is BLAKE3-256 over the JCS form (spec/canonical-serialization.md). Operation",
         "stand-ins (`op-*`) are illustrative stand-ins for the OpenAPI adapter's output and are replaced by real adapter records in",
         f"phase 4. The attester identity is a design-phase throwaway key: `{ATTESTER}`.", "",
         "| file | address | what |", "|---|---|---|"]
for f, h, what in FILES: lines.append(f"| `records/{f}.json` | `{h}` | {what} |")
lines += ["", "## Planner output", "", f"Provisioning plan waves: `{json.dumps(WAVES)}`", "", f"Scrub plan waves: `{json.dumps(SCRUB_WAVES)}`", "",
          "Test plan waves per dish:", ""] + [f"- `{d}`: `{json.dumps(w)}`" for d, w in HARNESS_WAVES.items()] + \
         ["", "## Selector candidates per need (step 3)", "", f"`{json.dumps(CANDIDATES)}`"]
(HERE / 'addresses.md').write_text("\n".join(lines) + "\n")
print(f"{len(FILES)} records written; provisioning plan {len(PLAN['steps'])} steps in {len(WAVES)} waves; scrub plan {len(SCRUB_PLAN['steps'])} steps in {len(SCRUB_WAVES)} waves")
for i, w in enumerate(WAVES): print(f"  wave {i}: {w}")
print("scrub:")
for i, w in enumerate(SCRUB_WAVES): print(f"  wave {i}: {w}")
