#!/usr/bin/env python3
"""nl-ingest-smithy — Smithy JSON AST service models (the AWS `awsJson` protocols) -> verified
Nova Lingua records.

The seventh ingestion adapter and the third DESCRIPTION-layer one (after `nl-ingest-openapi` and
`nl-ingest-graphql`). It reads a service as AWS publishes it — a Smithy model in JSON AST form —
and compiles one client function per operation of a service whose protocol is `awsJson1_0` or
`awsJson1_1`: ONE endpoint, every operation a POST, the operation named by the `X-Amz-Target`
header (`<ServiceShapeName>.<Operation>`), the input a JSON document, the output a JSON document.
Most of AWS (ECS, ECR, Cloud Map, Global Accelerator, Cloud Control, …) speaks this shape; it does
NOT project to OpenAPI (the `aws-sdk-poc` route needs `restJson1`), hence an adapter of its own.
What the shape dictates is the whole adapter:

  * The INPUT is caller data. A Smithy input structure nests freely (lists of structures, maps,
    unions) and the members a real call needs are mostly optional — a provisioning call is never
    the "minimal documented call". So every record takes the whole input as ONE `Json` parameter
    (`(base, input) -> …`) and serializes it with the `render_json` builtin (the GraphQL
    variables precedent — no string splicing, no new builtin). The model's `required` members
    are read, but as a CONTRACT the service enforces (below), not as the record's parameter list.

  * The EFFECT is `net.write`, for every operation. The validator classes an `http` call by
    method and every awsJson call is a POST; the model's `smithy.api#readonly` trait could
    refine that — but it is NOT reliably present (measured across the five models this adapter
    was built for: ECS marks 29 of 77 operations, Global Accelerator / Cloud Control / ECR / Cloud
    Map mark NONE). A trait the description may omit cannot license a weaker effect; the method
    rule stands and the records are honest about the wire. `readonly` (by trait, or declared by
    the operator with `--readonly <glob>`) licenses only what it can: OBSERVING the operation
    during ingestion, and the `query/lookup` intent tag.

  * NOTHING is spec-derivable. awsJson success is always 200 and the model does not describe the
    validation error, so a record exists ONLY through the live observation gate
    (`--verify-against <url>`): the body runs once, the observation is held to the model, and
    becomes the trace-attached, offline-replayable worked example (the GraphQL stance: a model
    licenses shapes; only an observation supplies a value). Which call an operation can be
    observed AT is decided by the model:

      - readonly, no required member  -> the empty input `{}` is a VALID call: observe it,
                                         expect 200, materialize the output projections;
      - any operation with a required member -> `{}` VIOLATES the model's own `required`
                                         contract, so the service MUST reject it before acting:
                                         observe the rejection (expect a non-2xx carrying an
                                         error `__type`), record the status the service answered.
                                         This is the effect-free call of a MUTATING verb — the
                                         gcp proposal-02 move (DELETE at the absent name) carried
                                         to creates: `CreateService {}` costs nothing and proves the
                                         record speaks the protocol. A 2xx here means the
                                         description lied — the gate FAILS LOUDLY, and for a
                                         mutating verb says an effect may have occurred;
      - readonly with required members -> the rejection above by default; `--observe-arg
                                         <Op>.input=<json>` names real server state (the OQ1 move)
                                         and observes the 200 + projections instead;
      - mutating, no required member   -> NO effect-free call exists: REFUSED. `--observe-effect
                                         <Op>.input=<json>` is the explicit opt-in that performs
                                         the effect ONCE as the worked example (the ingestion is
                                         then the first provisioning step; the record carries the
                                         operator's values and is theirs to publish or not).

  * Output projections (readonly operations, from a 200 observation): the whole output document
    (`<Op>Output`, `Maybe Json`) plus one typed projection per top-level member the pattern language can narrow
    soundly — string/enum/blob -> `Maybe string`, boolean -> `Maybe bool`, structure/list/map/
    union/document -> `Maybe Json`; numeric and timestamp members are NOTED, never projected (JNum
    carries int or float; awsJson timestamps are epoch-seconds numbers). The observed document is
    held to the declared member types; a `required` output member absent or null refuses; an
    optional member's explicit `null` reads as absent (the aws-sdk-poc finding-7 decision).

  * Authentication is NOT the record's business (aws-sdk-poc finding 5): SigV4 is a computed
    signature over the request, which no static header can carry, so `base` is the operator's
    signing entry point (a local proxy) and the records are identity-free; traces replay without
    credentials. Region and service live in `base` too — the same record provisions in every
    region.

Requires only python3 and the built `nl-validator` (sibling build, the quickstart's fetched binary,
or `NL_VALIDATOR`). Reuses `ingest-common` so records agree byte-for-byte with every other adapter
on canonical form and content-hash.
"""

import argparse
import fnmatch
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from urllib.parse import urlparse

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(_HERE, "..", "ingest-common")))
from nl_body import b_app, b_field, b_let, b_lit, b_var, b_variant  # noqa: E402
from nl_core import build_v2_record, canonicalize, sanitize_hint  # noqa: E402

STRING = {"kind": "builtin", "name": "string"}
INT = {"kind": "builtin", "name": "int"}
BOOL = {"kind": "builtin", "name": "bool"}
JSON_T = {"kind": "builtin", "name": "Json"}
MAYBE_JSON = {"kind": "sum", "variants": [{"tag": "Just", "type": JSON_T}, {"tag": "None"}]}
MAYBE_STRING = {"kind": "sum", "variants": [{"tag": "Just", "type": STRING}, {"tag": "None"}]}
MAYBE_BOOL = {"kind": "sum", "variants": [{"tag": "Just", "type": BOOL}, {"tag": "None"}]}
NONE_V = {"kind": "variant", "tag": "None"}
UNIT = "smithy.api#Unit"
PROTOCOLS = {"aws.protocols#awsJson1_0": "application/x-amz-json-1.0",
             "aws.protocols#awsJson1_1": "application/x-amz-json-1.1"}
BLOB_THRESHOLD_DEFAULT = 65536
# Error `__type`s that refuse the REQUEST, not the input: identity, signature, throttling, the
# target itself. A rejection carrying one of these says nothing about the model's `required`
# contract (the service never looked at the input), so it cannot be the observed rejection.
# Measured live: an out-of-boundary ECS ListClusters answers 400 AccessDeniedException — the
# same status a validation rejection carries.
_NOT_AN_INPUT_REJECTION = {
    "AccessDeniedException", "AccessDenied", "UnauthorizedException", "UnrecognizedClientException",
    "InvalidSignatureException", "IncompleteSignatureException", "IncompleteSignature",
    "MissingAuthenticationTokenException", "MissingAuthenticationToken", "ExpiredTokenException",
    "InvalidClientTokenId", "ThrottlingException", "TooManyRequestsException", "RequestLimitExceeded",
    "UnknownOperationException", "SerializationException", "InvalidAction", "ServiceUnavailableException",
    "InternalFailure", "InternalServerException", "ProxyRouteError",
}

# Output members: what a typed projection can narrow soundly by pattern, by Smithy shape type.
_NARROW = {"string": "string", "enum": "string", "blob": "string", "boolean": "bool"}
_AS_JSON = {"structure", "list", "set", "map", "union", "document"}
_NUMERIC = {"integer", "long", "short", "byte", "float", "double", "bigInteger", "bigDecimal", "intEnum",
            "timestamp"}


def _find_validator():
    """`NL_VALIDATOR` if set; else the sibling cargo build; else the binary quickstart.sh fetched
    into the repo's `.quickstart/` — so a stranger who ran the quickstart needs no env var."""
    if os.environ.get("NL_VALIDATOR"):
        return os.environ["NL_VALIDATOR"]
    build = os.path.normpath(os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator"))
    fetched = os.path.normpath(os.path.join(_HERE, "..", "..", ".quickstart", "nl-validator"))
    return build if os.path.exists(build) or not os.path.exists(fetched) else fetched


_VALIDATOR = _find_validator()


# ---------------------------------------------------------------------------------------------
# Model loading
# ---------------------------------------------------------------------------------------------

def load_model(path):
    """A Smithy JSON AST (`{"smithy": "2.0", "shapes": {…}}`) holding exactly one service shape
    whose protocol is an awsJson one. Locally complete by construction — the AWS models ship as
    single files; no network at ingestion time."""
    with open(path) as f:
        doc = json.load(f)
    shapes = doc.get("shapes") if isinstance(doc, dict) else None
    if not isinstance(shapes, dict):
        raise SystemExit(f"{path}: not a Smithy JSON AST (no `shapes` map)")
    services = [k for k, v in shapes.items() if v.get("type") == "service"]
    if len(services) != 1:
        raise SystemExit(f"{path}: expected exactly one service shape, found {len(services)}")
    sid = services[0]
    protos = [t for t in shapes[sid].get("traits", {}) if t in PROTOCOLS]
    if len(protos) != 1:
        others = [t for t in shapes[sid].get("traits", {}) if t.startswith("aws.protocols#")]
        raise SystemExit(f"{path}: service {sid} speaks {others or 'no declared protocol'} — this adapter "
                         f"compiles {sorted(PROTOCOLS)} only (restJson1 goes through nl-ingest-openapi; "
                         "the Query/XML protocols have no route)")
    return {"shapes": shapes, "service": sid, "protocol": protos[0]}


def service_facts(model):
    """What the wire needs from the service shape: the X-Amz-Target prefix (the service shape's
    NAME), the Content-Type (by protocol), and a short id for hints/tags (`aws.api#service.sdkId`
    when present, else the shape name)."""
    sid = model["service"]
    traits = model["shapes"][sid].get("traits", {})
    sdk = (traits.get("aws.api#service") or {}).get("sdkId") or sid.split("#")[1]
    short = re.sub(r"[^a-z0-9]+", "-", sdk.lower()).strip("-")
    return {"target_prefix": sid.split("#")[1], "content_type": PROTOCOLS[model["protocol"]],
            "short": short}


def collect_operations(model):
    """Every operation the service reaches: bound directly, or through its resources (lifecycle
    operations, `operations`, `collectionOperations`, child resources). Sorted by name."""
    shapes = model["shapes"]
    acc, seen = set(), set()

    def walk(shape_id):
        sh = shapes.get(shape_id) or {}
        for ref in sh.get("operations", []) + sh.get("collectionOperations", []):
            acc.add(ref["target"])
        for key in ("create", "put", "read", "update", "delete", "list"):
            if key in sh:
                acc.add(sh[key]["target"])
        for ref in sh.get("resources", []):
            if ref["target"] not in seen:
                seen.add(ref["target"])
                walk(ref["target"])

    walk(model["service"])
    return sorted(acc, key=lambda s: s.split("#")[1])


# The Smithy prelude: shapes every model may reference without defining (`smithy.api#String` …).
_PRELUDE = {f"smithy.api#{n}": t for n, t in [
    ("String", "string"), ("Blob", "blob"), ("Boolean", "boolean"), ("PrimitiveBoolean", "boolean"),
    ("Byte", "byte"), ("PrimitiveByte", "byte"), ("Short", "short"), ("PrimitiveShort", "short"),
    ("Integer", "integer"), ("PrimitiveInteger", "integer"), ("Long", "long"), ("PrimitiveLong", "long"),
    ("Float", "float"), ("PrimitiveFloat", "float"), ("Double", "double"), ("PrimitiveDouble", "double"),
    ("BigInteger", "bigInteger"), ("BigDecimal", "bigDecimal"), ("Timestamp", "timestamp"),
    ("Document", "document"), ("Unit", "unit")]}


def _shape(shapes, target):
    """The shape a member targets — a model shape, or a prelude shape (synthesized, no members)."""
    sh = shapes.get(target)
    if sh is None and target in _PRELUDE:
        return {"type": _PRELUDE[target]}
    return sh or {}


def _members(shapes, target):
    if not target or target == UNIT:
        return {}
    return _shape(shapes, target).get("members") or {}


def _required(members):
    return sorted(m for m, md in members.items() if "smithy.api#required" in (md.get("traits") or {}))


def _enum_values(shape):
    vals = []
    for name, md in (shape.get("members") or {}).items():
        vals.append((md.get("traits") or {}).get("smithy.api#enumValue", name))
    return sorted(vals)


# ---------------------------------------------------------------------------------------------
# Body synthesis
# ---------------------------------------------------------------------------------------------

def s_lit(s):
    return b_lit({"kind": "string", "value": s})


def curried_app(fn, *args):
    node = fn
    for a in args:
        node = b_app(node, [a])
    return node


def _case_bool(test, then_expr, else_expr):
    return {"kind": "case", "scrutinee": test,
            "arms": [{"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": then_expr},
                     {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": False}}, "body": else_expr}]}


def _case_tag(scrutinee, tag, bind, body):
    """`case s of { Tag(x) => body; _ => None }`."""
    return {"kind": "case", "scrutinee": scrutinee,
            "arms": [{"pattern": {"kind": "variant", "tag": tag, "payload": {"kind": "bind", "name": bind}},
                      "body": body},
                     {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}


def _param_name(raw):
    name = re.sub(r"[^a-zA-Z0-9]", "_", raw)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name).lower()
    name = re.sub(r"_+", "_", name).strip("_")
    if not name or not name[0].isalpha():
        name = "p_" + name
    return name


def _intent_ext(lead, short, name):
    tag = f"{lead}/{short}-{_param_name(name).replace('_', '-')}"
    return [tag] if len(tag) <= 64 else []


def call_expr(facts, op_name):
    """`http "POST" base {Content-Type, X-Amz-Target} (render_json input)` — the one call every
    record makes; the target header is a spec-time literal, the body is the caller's `Json`."""
    headers = curried_app(b_var("map_put"), s_lit("X-Amz-Target"), s_lit(f"{facts['target_prefix']}.{op_name}"),
                          b_var("map_empty"))
    headers = curried_app(b_var("map_put"), s_lit("Content-Type"), s_lit(facts["content_type"]), headers)
    body = curried_app(b_var("render_json"), b_var("input"))
    return curried_app(b_var("http"), s_lit("POST"), b_var("base"), headers, body)


def status_body(call):
    """`\\base input -> let r = call in r.status` — the leaf record: what the service answered."""
    return {"kind": "lambda", "params": [{"name": "base"}, {"name": "input"}],
            "body": b_let("r", call, b_field(b_var("r"), "status"))}


def projection_body(call, member=None, kind="json"):
    """`\\base input -> let r = call in` status-guarded (200 — awsJson's only success status)
    `parse_json r.body` -> `JObj m` -> the whole document, or `map_get "<member>" m` narrowed by
    constructor for a typed member projection."""
    if member is None:
        get = {"kind": "variant", "tag": "Just", "payload": b_variant("JObj", b_var("m"))}
    else:
        leaf = curried_app(b_var("map_get"), s_lit(member), b_var("m"))
        if kind == "string":
            get = {"kind": "case", "scrutinee": leaf,
                   "arms": [{"pattern": {"kind": "variant", "tag": "Just", "payload": {"kind": "bind", "name": "w"}},
                             "body": _case_tag(b_var("w"), "JStr", "s",
                                               {"kind": "variant", "tag": "Just", "payload": b_var("s")})},
                            {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}
        elif kind == "bool":
            get = {"kind": "case", "scrutinee": leaf,
                   "arms": [{"pattern": {"kind": "variant", "tag": "Just", "payload": {"kind": "bind", "name": "w"}},
                             "body": _case_tag(b_var("w"), "JBool", "b",
                                               {"kind": "variant", "tag": "Just", "payload": b_var("b")})},
                            {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}
        else:
            get = leaf
    on_status = {"kind": "case", "scrutinee": curried_app(b_var("parse_json"), b_field(b_var("r"), "body")),
                 "arms": [{"pattern": {"kind": "variant", "tag": "Just", "payload": {"kind": "bind", "name": "j"}},
                           "body": _case_tag(b_var("j"), "JObj", "m", get)},
                          {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}
    return {"kind": "lambda", "params": [{"name": "base"}, {"name": "input"}],
            "body": b_let("r", call, _case_bool(
                curried_app(b_var("eq"), b_field(b_var("r"), "status"), b_lit({"kind": "nat", "value": 200})),
                on_status, NONE_V))}


# ---------------------------------------------------------------------------------------------
# Values: JSON <-> the evaluator's Json-sum value AST
# ---------------------------------------------------------------------------------------------

def json_to_value(x):
    if x is None:
        return {"kind": "variant", "tag": "JNull"}
    if isinstance(x, bool):
        return {"kind": "variant", "tag": "JBool", "payload": {"kind": "bool", "value": x}}
    if isinstance(x, int):
        return {"kind": "variant", "tag": "JNum", "payload": {"kind": "int", "value": x}}
    if isinstance(x, float):
        return {"kind": "variant", "tag": "JNum", "payload": {"kind": "float", "value": x}}
    if isinstance(x, str):
        return {"kind": "variant", "tag": "JStr", "payload": {"kind": "string", "value": x}}
    if isinstance(x, list):
        return {"kind": "variant", "tag": "JList", "payload": {"kind": "list", "elems": [json_to_value(e) for e in x]}}
    if isinstance(x, dict):
        return {"kind": "variant", "tag": "JObj",
                "payload": {"kind": "map", "entries": [{"key": k, "value": json_to_value(x[k])} for k in sorted(x)]}}
    raise ValueError(f"not a JSON value: {x!r}")


def observed_conforms(shapes, target, doc, path):
    """Hold an OBSERVED output document (parsed JSON) to its declared structure: every `required`
    member present and non-null; every present member of the declared shape type (string/enum/
    blob -> string with enum values in the declared set; boolean; numeric kinds -> number;
    structure/union/map -> object, recursively; list/set -> list of conforming elements; document
    -> anything; timestamp -> unconstrained, its wire form is a trait the adapter does not read).
    An optional member's explicit `null` reads as absent. Returns (ok, why)."""
    if not target or target == UNIT:
        return True, ""
    sh = _shape(shapes, target)
    t = sh.get("type")
    if t in ("structure", "union"):
        if not isinstance(doc, dict):
            return False, f"`{path}` is not an object"
        for m, md in (sh.get("members") or {}).items():
            req = "smithy.api#required" in (md.get("traits") or {})
            if m not in doc or doc[m] is None:
                if req:
                    return False, f"required member `{path}.{m}` {'is null' if m in doc else 'absent'}"
                continue
            ok, why = observed_conforms(shapes, md["target"], doc[m], f"{path}.{m}")
            if not ok:
                return ok, why
        return True, ""
    if t == "map":
        if not isinstance(doc, dict):
            return False, f"`{path}` is not an object"
        for k, v in doc.items():
            if v is None:
                continue
            ok, why = observed_conforms(shapes, sh["value"]["target"], v, f"{path}[{k!r}]")
            if not ok:
                return ok, why
        return True, ""
    if t in ("list", "set"):
        if not isinstance(doc, list):
            return False, f"`{path}` is not a list"
        for i, e in enumerate(doc):
            ok, why = observed_conforms(shapes, sh["member"]["target"], e, f"{path}[{i}]")
            if not ok:
                return ok, why
        return True, ""
    if t in ("string", "blob"):
        return (True, "") if isinstance(doc, str) else (False, f"`{path}` is not a string")
    if t == "enum":
        if not isinstance(doc, str):
            return False, f"`{path}` is not a string"
        if doc not in _enum_values(sh):
            return False, f"`{path}` = {doc!r} is not a declared value of {target.split('#')[1]}"
        return True, ""
    if t == "boolean":
        return (True, "") if isinstance(doc, bool) else (False, f"`{path}` is not a boolean")
    if t in _NUMERIC - {"timestamp"}:
        if isinstance(doc, bool) or not isinstance(doc, (int, float)):
            return False, f"`{path}` is not a number"
        return True, ""
    return True, ""  # timestamp (wire form is trait-chosen) / document / unknown: unconstrained


# ---------------------------------------------------------------------------------------------
# Operation -> plan (what the gate will do) + pending records
# ---------------------------------------------------------------------------------------------

def _wants(bindings, op):
    return bindings.get(op)


def plan_operation(model, facts, op_id, *, readonly_globs=(), observe=None, observe_effect=None):
    """One operation -> ("ok", plan, notes) or ("skip", name, reason). The plan says which call the
    observation gate makes and what it expects; `pending` lists the records it may mint."""
    shapes = model["shapes"]
    name = op_id.split("#")[1]
    op = shapes[op_id]
    notes = []
    traits = op.get("traits") or {}
    readonly = "smithy.api#readonly" in traits
    declared = any(fnmatch.fnmatchcase(name, g) for g in readonly_globs)
    if declared and not readonly:
        readonly = True
        notes.append(f"{name}: readonly by OPERATOR DECLARATION (--readonly), not by trait — the model is "
                     "silent; the declaration licenses observing it")
    inp = (op.get("input") or {}).get("target")
    out = (op.get("output") or {}).get("target")
    members = _members(shapes, inp)
    required = _required(members)
    bound = (observe or {}).get(name)
    effect_bound = (observe_effect or {}).get(name)
    if bound is not None and effect_bound is not None:
        return "skip", name, "bound by BOTH --observe-arg and --observe-effect — pick one"
    # Which call is observed, and what the model says it must answer.
    if effect_bound is not None:
        if readonly:
            return "skip", name, "--observe-effect on a readonly operation — use --observe-arg (no effect to opt into)"
        try:
            inp_value = json.loads(effect_bound)
        except ValueError as e:
            return "skip", name, f"--observe-effect value is not JSON: {e}"
        if not isinstance(inp_value, dict):
            return "skip", name, "--observe-effect value must be a JSON object (the operation's input)"
        missing = [m for m in required if m not in inp_value]
        if missing:
            return "skip", name, f"--observe-effect input lacks required member(s) {missing} — the service would reject it"
        mode, expect = "effect", "2xx"
        notes.append(f"{name}: EFFECT OPT-IN — the gate performs this mutating operation ONCE at the operator's "
                     "input; the observation is the worked example and carries the operator's values")
    elif bound is not None:
        if not readonly:
            return "skip", name, ("--observe-arg names a MUTATING operation (no readonly trait or declaration) — an "
                                  "observation must not create state; --observe-effect is the explicit opt-in")
        try:
            inp_value = json.loads(bound)
        except ValueError as e:
            return "skip", name, f"--observe-arg value is not JSON: {e}"
        if not isinstance(inp_value, dict):
            return "skip", name, "--observe-arg value must be a JSON object (the operation's input)"
        missing = [m for m in required if m not in inp_value]
        if missing:
            return "skip", name, f"--observe-arg input lacks required member(s) {missing} — the service would reject it"
        mode, expect = "arg", "2xx"
    elif required:
        inp_value, mode, expect = {}, "reject", "reject"
        notes.append(f"{name}: observed at `{{}}`, which violates required member(s) {required} — the service "
                     "must reject it before acting; the record's example is that rejection"
                     + (" (readonly: --observe-arg <Op>.input=<json> observes a real 200 instead)" if readonly else ""))
    elif readonly:
        inp_value, mode, expect = {}, "empty", "2xx"
    else:
        return "skip", name, ("mutating operation with NO required member — the empty input is a VALID call, so "
                              "no effect-free observation exists; --observe-effect <Op>.input=<json> opts into "
                              "performing it once")
    args = [{"kind": "string", "value": "{{base}}"}, json_to_value(inp_value)]
    call = call_expr(facts, name)
    lead = "query/lookup" if readonly else "io/network/http"
    base_tags = ["io", "io/network/http"] + (["query/lookup"] if readonly else [])
    pending = [{
        "name": name, "hint": _param_name(name), "member": None, "kind": "status",
        "type_ast": {"kind": "fn", "params": [STRING, JSON_T], "result": INT},
        "body_ast": status_body(call), "intent": base_tags + _intent_ext(lead, facts["short"], name),
    }]
    # Output projections: licensed for a readonly operation observed at a success.
    if readonly and expect == "2xx" and out and out != UNIT:
        pending.append({
            "name": name + "Output", "hint": _param_name(name + "Output"), "member": None, "kind": "json",
            "type_ast": {"kind": "fn", "params": [STRING, JSON_T], "result": MAYBE_JSON},
            "body_ast": projection_body(call), "intent": base_tags + ["parse"] + _intent_ext("parse", facts["short"], name + "Output"),
        })
        taken = {p["name"].lower() for p in pending}
        for m, md in (_members(shapes, out)).items():
            t = _shape(shapes, md["target"]).get("type")
            if t in _NARROW:
                kind = _NARROW[t]
                rtype = MAYBE_STRING if kind == "string" else MAYBE_BOOL
            elif t in _AS_JSON:
                kind, rtype = "json", MAYBE_JSON
            elif t in _NUMERIC:
                notes.append(f"{name}: output member `{m}` ({t}) not projected (JNum carries int or float — "
                             "a typed numeric promise cannot be narrowed soundly by pattern)")
                continue
            else:
                continue
            pname = f"{name}{m[:1].upper()}{m[1:]}"
            if pname.lower() in taken:  # a member spelled like the whole-document record (`output`)
                notes.append(f"{name}: output member `{m}` not projected — its record name would collide with "
                             f"`{name}Output` (the whole document carries it)")
                continue
            taken.add(pname.lower())
            pending.append({
                "name": pname, "hint": _param_name(pname), "member": m, "kind": kind,
                "type_ast": {"kind": "fn", "params": [STRING, JSON_T], "result": rtype},
                "body_ast": projection_body(call, m, kind),
                "intent": base_tags + ["parse"] + _intent_ext("parse", facts["short"], pname),
            })
    plan = {"name": name, "readonly": readonly, "required": required, "input": inp_value, "mode": mode,
            "expect": expect, "output": out, "args": args, "pending": pending}
    return "ok", plan, notes


def walk(model, *, readonly_globs=(), observe=None, observe_effect=None):
    facts = service_facts(model)
    plans, skipped, notes = [], [], []
    names = set()
    for op_id in collect_operations(model):
        names.add(op_id.split("#")[1])
        st, a, b = plan_operation(model, facts, op_id, readonly_globs=readonly_globs, observe=observe,
                                  observe_effect=observe_effect)
        if st == "ok":
            plans.append(a)
            notes.extend(b)
        else:
            skipped.append((a, b))
    unheeded = sorted((set(observe or {}) | set(observe_effect or {})) - names)
    report = {"operations": len(names), "planned": len(plans), "refused": len(skipped),
              "readonly": sum(1 for p in plans if p["readonly"]),
              "projections": sum(len(p["pending"]) for p in plans)}
    return facts, plans, skipped, notes, report, unheeded


# ---------------------------------------------------------------------------------------------
# The observation gate
# ---------------------------------------------------------------------------------------------

def blobify_example(ex, out_dir, threshold):
    if threshold is None or "result" not in ex:
        return None
    data = canonicalize(ex["result"])
    if len(data) <= threshold:
        return None
    sha = hashlib.sha256(data).hexdigest()
    with open(os.path.join(out_dir, f"blob-{sha}.json"), "wb") as f:
        f.write(data)
    ex["result_blob"] = {"sha256": sha, "bytes": len(data)}
    del ex["result"]
    return sha


def _write_json(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f, indent=2)


def _traced_call(trace_path):
    try:
        with open(trace_path) as f:
            trace = json.load(f)
    except (OSError, ValueError):
        return None, None
    ops = trace.get("ops") or []
    if not ops:
        return None, None
    status = body = None
    for f in (ops[0].get("result") or {}).get("fields", []):
        if f.get("name") == "status":
            status = (f.get("value") or {}).get("value")
        elif f.get("name") == "body":
            body = (f.get("value") or {}).get("value")
    return status, body


def _run_body(p, plan, out_dir, base_url, replay_from=None):
    """Evaluate one pending record's body at the plan's arguments: live (trace out) or by replay
    of the sibling's trace (the same request is never re-issued). Returns (ok, value|message,
    trace_path)."""
    fbase = sanitize_hint(p["name"])
    bp = os.path.join(out_dir, f"body-{fbase}.json")
    _write_json(bp, p["body_ast"])
    args = [dict(a) for a in plan["args"]]
    args[0] = {"kind": "string", "value": base_url}
    argfiles = []
    for j, a in enumerate(args):
        ap = os.path.join(out_dir, f".arg-{fbase}-{j}.json")
        _write_json(ap, a)
        argfiles.append(ap)
    trace_path = os.path.join(out_dir, f"trace-{fbase}-0.json")
    host = urlparse(base_url).hostname or ""
    cmd = [_VALIDATOR, "eval", bp]
    for ap in argfiles:
        cmd += ["--arg", ap]
    cmd += ["--grant", f"net.write@{host}"]
    if replay_from is not None:
        with open(replay_from, "rb") as src, open(trace_path, "wb") as dst:
            dst.write(src.read())
        cmd += ["--replay", trace_path]
    else:
        cmd += ["--trace-out", trace_path]
    r = subprocess.run(cmd, capture_output=True, text=True)
    for ap in argfiles:
        os.unlink(ap)
    if r.returncode != 0:
        return False, f"evaluation failed: {(r.stderr or '').strip()}", trace_path
    return True, json.loads(r.stdout), trace_path


def _mint(p, plan, got, trace_path, out_dir, base_url, blob_threshold, extra_notes=()):
    trc = subprocess.run([_VALIDATOR, "hash", trace_path], capture_output=True, text=True).stdout.strip()
    if not trc.startswith("trc_"):
        return False, f"trace did not hash to a trc_… address: {trc!r}"
    args = [dict(a) for a in plan["args"]]
    args[0] = {"kind": "string", "value": base_url}
    example = {"args": args, "result": got, "trace": trc}
    blobify_example(example, out_dir, blob_threshold)
    record = build_v2_record(name=p["name"], type_ast=p["type_ast"], examples=[example], body_text=p["body_ast"],
                             module_name=None, extra_hints=[p["hint"]], effects=["net.write"], terminates="always",
                             intent_tags=p["intent"], complexity="O(n)")
    rp = os.path.join(out_dir, f"{sanitize_hint(p['name'])}.v0.2.json")
    _write_json(rp, record)
    return True, record


def observe_plan(model, plan, out_dir, base_url, blob_threshold=BLOB_THRESHOLD_DEFAULT):
    """Run the plan's ONE live call through the leaf record, judge it against what the model
    says that call must answer, then mint the leaf and (on a success) every licensed projection
    by replaying the same trace. Returns (results, live_message) where results is a list of
    (name, ok, message, record_or_None)."""
    shapes = model["shapes"]
    leaf = plan["pending"][0]
    ok, got, trace_path = _run_body(leaf, plan, out_dir, base_url)
    if not ok:
        return [(leaf["name"], False, got, None)], "no call"
    status, body = _traced_call(trace_path)
    try:
        doc = json.loads(body) if body is not None else None
    except ValueError:
        doc = None
    if plan["expect"] == "reject":
        if status is None or 200 <= status < 300:
            effect = ("" if plan["readonly"] else " — a MUTATING operation accepted an input violating its own "
                      "`required` contract; an effect MAY HAVE OCCURRED, check the service")
            return [(leaf["name"], False, f"the service answered {status} to `{{}}` although member(s) "
                     f"{plan['required']} are required — the description's contract does not hold{effect}", None)], f"{status}"
        if not isinstance(doc, dict) or "__type" not in doc:
            return [(leaf["name"], False, f"the service answered {status} without an awsJson error document "
                     "(`__type`) — a transport refusal, not the protocol's rejection; nothing was observed", None)], f"{status}"
        etype = str(doc["__type"]).split("#")[-1].split(":")[0]
        if etype in _NOT_AN_INPUT_REJECTION or status >= 500:
            return [(leaf["name"], False, f"the service answered {status} {etype} — the REQUEST was refused "
                     "(identity, signature, throttling, target, or a server fault), so the input was never "
                     "validated; that is not the rejection the model promises and nothing was observed", None)], f"{status} {etype}"
        live = f"{status} {etype}"
        m_ok, rec = _mint(leaf, plan, got, trace_path, out_dir, base_url, blob_threshold)
        return [(leaf["name"], m_ok, live if m_ok else rec, rec if m_ok else None)], live
    # expect 2xx
    if status != 200:
        why = f"the service answered {status}" + (f" {doc.get('__type', '').split('#')[-1]}" if isinstance(doc, dict) else "")
        return [(leaf["name"], False, f"{why}, not 200 — the observation did not succeed; nothing was recorded", None)], f"{status}"
    if not isinstance(doc, dict):
        return [(leaf["name"], False, "the 200 response body is not a JSON object — no document was obtained", None)], "200 non-JSON"
    ok, why = observed_conforms(shapes, plan["output"], doc, "output")
    if not ok:
        return [(leaf["name"], False, f"observed output violates the declared shape: {why}", None)], "200 nonconforming"
    results = []
    m_ok, rec = _mint(leaf, plan, got, trace_path, out_dir, base_url, blob_threshold)
    results.append((leaf["name"], m_ok, "200" if m_ok else rec, rec if m_ok else None))
    for p in plan["pending"][1:]:
        ok, got_p, tp = _run_body(p, plan, out_dir, base_url, replay_from=trace_path)
        if not ok:
            results.append((p["name"], False, got_p, None))
            continue
        is_none = isinstance(got_p, dict) and got_p.get("tag") == "None"
        if p["member"] is None and is_none:
            results.append((p["name"], False, "the whole-document projection answered None on a 200 JSON object — "
                            "the body does not parse as the record parses it", None))
            continue
        m_ok, rec = _mint(p, plan, got_p, tp, out_dir, base_url, blob_threshold)
        results.append((p["name"], m_ok, ("replayed" + (" None" if is_none else "")) if m_ok else rec, rec if m_ok else None))
    return results, "200"


def certify(record_path, body_path, out_dir):
    r = subprocess.run([_VALIDATOR, "certify", record_path, "--body", body_path, "--records", out_dir],
                       capture_output=True, text=True)
    return r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip()


def verify_examples(record_path, out_dir):
    """Replay with no grants beyond the record's own and no service: the offline check any commons
    consumer can perform."""
    r = subprocess.run([_VALIDATOR, "run", record_path, "--records", out_dir], capture_output=True, text=True)
    return r.returncode == 0, (r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip())


# ---------------------------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------------------------

def parse_bindings(items, flag):
    """`<Op>.input=<json>` -> {Op: json_text}. The parameter is always `input` (the one Json
    parameter every record takes); the grammar matches the other adapters' `--observe-arg`."""
    out = {}
    for item in items or []:
        key, sep, value = item.partition("=")
        op, dot, param = key.partition(".")
        if not sep or not dot or param != "input" or not op:
            raise SystemExit(f"{flag} expects <Operation>.input=<json>, got {item!r}")
        if op in out:
            raise SystemExit(f"{flag} binds {op} twice")
        out[op] = value
    return out


def _result_text(p):
    r = p["type_ast"]["result"]
    if r == INT:
        return "int"
    if r == MAYBE_JSON:
        return "Maybe Json"
    return "Maybe " + r["variants"][0]["type"]["name"]


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__.split("\n\n")[0],
        epilog="Needs the nl-validator binary: the sibling tooling/validator build, else the one quickstart.sh "
               "fetched into .quickstart/, else set NL_VALIDATOR=/path/to/nl-validator. Output: <op>.v0.2.json "
               "records, body-<op>.json bodies, trace-<op>-0.json traces, where <op> is the operation name "
               "LOWERCASED (ListClusters -> listclusters.v0.2.json); every record is (base: string, input: Json); "
               "AWS needs a SigV4-signing entry point as --verify-against (the records carry no identity); "
               "certify one by hand with `nl-validator certify <record> --body <body> --records <out>`; publish "
               "with tooling/commons-node/publish_records.py <out>.")
    ap.add_argument("model", help="a Smithy JSON AST model (one awsJson1_0/1_1 service)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--verify-against", default=None, metavar="URL",
                    help="the service endpoint (for AWS: the operator's signing proxy): the observation gate")
    ap.add_argument("--readonly", action="append", default=[], metavar="GLOB",
                    help="declare operations readonly by name glob (e.g. 'List*') where the model carries no "
                         "`readonly` trait — the operator's attestation; licenses observing them, not a weaker effect")
    ap.add_argument("--observe-arg", action="append", default=[], metavar="OP.input=JSON",
                    help="observe a READONLY operation at this input (real server state the model cannot name)")
    ap.add_argument("--observe-effect", action="append", default=[], metavar="OP.input=JSON",
                    help="OPT-IN: perform a MUTATING operation once at this input as its worked example")
    ap.add_argument("--blob-threshold", type=int, default=BLOB_THRESHOLD_DEFAULT)
    ap.add_argument("--pace", type=float, default=0.0, metavar="SECONDS",
                    help="minimum spacing between live calls (a service's rate limit is the operator's to respect)")
    a = ap.parse_args(argv)

    model = load_model(a.model)
    observe = parse_bindings(a.observe_arg, "--observe-arg")
    observe_effect = parse_bindings(a.observe_effect, "--observe-effect")
    if (observe or observe_effect) and not a.verify_against:
        ap.error("--observe-arg/--observe-effect require --verify-against (an observation needs a service)")
    facts, plans, skipped, notes, report, unheeded = walk(model, readonly_globs=a.readonly, observe=observe,
                                                          observe_effect=observe_effect)
    print(f"service {model['service']} ({model['protocol'].split('#')[1]}; target prefix {facts['target_prefix']})")
    for name, why in skipped:
        print(f"skip {name}: {why}")
    for n in notes:
        print(f"note {n}")
    if unheeded:
        print(f"refuse: bindings name operation(s) {unheeded} the service does not declare")
        sys.exit(2)
    for k, v in report.items():
        print(f"report {k}={v}")
    os.makedirs(a.out, exist_ok=True)
    if not a.verify_against:
        for plan in plans:
            how = {"empty": "observe `{}` -> 200", "reject": "observe `{}` -> rejection",
                   "arg": "observe operator input -> 200", "effect": "PERFORM operator input -> 200"}[plan["mode"]]
            for p in plan["pending"]:
                print(f"licensed {p['name']} : (string, Json) -> {_result_text(p)}  [net.write]  {how}")
        print(f"summary: {report['projections']} records licensed across {report['planned']} operations, 0 "
              "materialized (no --verify-against — a model licenses shapes; only an observation supplies a value)")
        return
    ok_all = True
    made = live_calls = 0
    for plan in plans:
        if live_calls and a.pace > 0:
            time.sleep(a.pace)
        live_calls += 1
        results, live = observe_plan(model, plan, a.out, a.verify_against, a.blob_threshold)
        for name, ok, msg, rec in results:
            if not ok:
                print(f"{name}: observation-gate=FAIL {msg}")
                ok_all = False
                continue
            made += 1
            rp = os.path.join(a.out, f"{sanitize_hint(name)}.v0.2.json")
            bp = os.path.join(a.out, f"body-{sanitize_hint(name)}.json")
            c_ok, c_msg = certify(rp, bp, a.out)
            v_ok, v_msg = verify_examples(rp, a.out)
            ex = rec["examples"][0]
            by_addr = f"BY-ADDRESS({ex['result_blob']['bytes']} bytes)" if "result_blob" in ex else "inline"
            print(f"{name}: observation-gate=OK({msg}) certify={'OK' if c_ok else 'FAIL'} "
                  f"replay={'OK' if v_ok else 'FAIL'} example={by_addr} trace={ex['trace'][:16]}… "
                  f"{rec['hash'][:16]}… -> {sanitize_hint(name)}.v0.2.json")
            if not (c_ok and v_ok):
                print(f"  {c_msg if not c_ok else v_msg}")
                ok_all = False
    print(f"summary: {made} records materialized of {report['projections']} licensed from {live_calls} live calls; "
          f"{'all certified + replayed' if ok_all else 'FAILURES above'}")
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
