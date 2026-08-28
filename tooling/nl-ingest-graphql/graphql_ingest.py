#!/usr/bin/env python3
"""nl-ingest-graphql — GraphQL introspection schemas -> verified Nova Lingua records.

The sixth ingestion adapter and the second DESCRIPTION-layer one (after `nl-ingest-openapi`): it
reads a GraphQL schema as the service itself describes it — the result of the standard
introspection query (`__schema`), saved locally — and compiles one client function per root
`Query` field. Where an OpenAPI description names operations, verbs, paths and documented
statuses, an introspection schema names TYPES: root fields, their arguments (with nullability),
and the shape of what they return. That difference is the whole adapter:

  * The request is a spec-time DOCUMENT. The adapter derives a deterministic selection set from
    the return type (every argument-free scalar/enum leaf, object leaves to `--select-depth`) and
    a `query Q($a: T!, …) { field(a: $a, …) { <selection> } }` document that is a string LITERAL
    in the record. Caller data never enters the document: it rides as GraphQL VARIABLES, built
    as a `Json` value (`JObj (map_put "code" (JStr code) map_empty)`) and serialized by the
    `render_json` builtin — so there is no string splicing anywhere, and no new builtin was
    needed (a zero-pull: the language already had the sound encoder — the `url_encode` argument
    of GW10, settled by composition this time).

  * The transport is a SERVER property the description does not declare. GraphQL-over-HTTP
    serves queries on GET (`?query=…&variables=…`) or POST (a JSON body); `--transport` picks,
    default `get`. Under GET the document is percent-encoded at generation time (spec-time
    literal, like a query-parameter NAME in the OpenAPI adapter) and the variables ride through
    `url_encode` at run time; the effect is `net.read`. Under POST the validator's method rule
    infers `net.write` for what the description calls a query — the record is honest about the
    wire and conservative about the semantics; the tax is measured, not hidden.

  * NOTHING is spec-derivable. A GraphQL response is `200 {"data": …}` for success AND for an
    absent name (`data.field = null`) AND for a validation failure (`errors`, no `data`) — the
    transport status carries no verdict, and the schema documents no values. So this adapter
    has no leaf "status" record and no spec-derived worked example at all: every record is a
    PROJECTION of `data.<field>` — the whole value (`Maybe Json`) plus one typed projection per
    scalar leaf of an object result (`String`/`ID`/enum -> `Maybe string`, `Boolean` ->
    `Maybe bool`; `Int`/`Float` noted, never projected — the `JNum` reasoning) — materialized
    ONLY through the live observation gate (`--verify-against`): the body runs once, the
    observed value is held to the declared type (nullability, list-ness, every selected leaf
    PRESENT — the GraphQL spec guarantees a selected field appears in the data — and typed, enum
    values in the declared set), and the observation becomes the worked example, trace-attached
    and offline-replayable. Without the gate the adapter prints a licensing report and writes
    nothing: a schema licenses shapes; it never supplies a value.

  * Read-only by rule. `Mutation` and `Subscription` root fields refuse (an observation must not
    create state during ingestion; a subscription has no request/response shape). A root field
    with required arguments needs `--observe-arg <field>.<arg>=<value>` — the operator names the
    server state the schema cannot; an argument-free field is constructible and observes on its
    own. A binding that touches nothing refuses loudly before any call.

Requires only python3 and the built `nl-validator` on the sibling `target/release` path. Reuses
`ingest-common` (BLAKE3+JCS core, body constructors) so records agree byte-for-byte with every
other adapter on canonical form and content-hash.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(_HERE, "..", "ingest-common")))
from nl_body import b_app, b_field, b_let, b_lit, b_var, b_variant  # noqa: E402
from nl_core import build_v2_record, canonicalize, sanitize_hint  # noqa: E402

STRING = {"kind": "builtin", "name": "string"}
INT = {"kind": "builtin", "name": "int"}
BOOL = {"kind": "builtin", "name": "bool"}
FLOAT = {"kind": "builtin", "name": "float"}
JSON_T = {"kind": "builtin", "name": "Json"}
MAYBE_JSON = {"kind": "sum", "variants": [{"tag": "Just", "type": JSON_T}, {"tag": "None"}]}
MAYBE_STRING = {"kind": "sum", "variants": [{"tag": "Just", "type": STRING}, {"tag": "None"}]}
MAYBE_BOOL = {"kind": "sum", "variants": [{"tag": "Just", "type": BOOL}, {"tag": "None"}]}
NONE_V = {"kind": "variant", "tag": "None"}
# The validator binary: `NL_VALIDATOR` if set (a prebuilt release, quickstart.sh), else the sibling build.
def _find_validator():
    """`NL_VALIDATOR` if set; else the sibling cargo build; else the binary quickstart.sh fetched
    into the repo's `.quickstart/` — so a stranger who ran the quickstart needs no env var."""
    if os.environ.get("NL_VALIDATOR"):
        return os.environ["NL_VALIDATOR"]
    build = os.path.normpath(os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator"))
    fetched = os.path.normpath(os.path.join(_HERE, "..", "..", ".quickstart", "nl-validator"))
    return build if os.path.exists(build) or not os.path.exists(fetched) else fetched


_VALIDATOR = _find_validator()
_UNRESERVED = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
BLOB_THRESHOLD_DEFAULT = 65536
SELECT_DEPTH_DEFAULT = 1

# The five built-in scalars: (parameter sort, Nova Lingua param type, Json constructor for the
# variables object). Anything else — a custom scalar, an input object, a list — is caller data the
# adapter cannot narrow: it rides as a `Json` parameter and is placed in the variables as-is.
_SCALAR_PARAMS = {
    "String": ("string", STRING, "JStr"),
    "ID": ("string", STRING, "JStr"),
    "Int": ("int", INT, "JNum"),
    "Float": ("float", FLOAT, "JNum"),
    "Boolean": ("bool", BOOL, "JBool"),
}
# Result leaves: what a typed field projection can narrow soundly by pattern.
_LEAF_NARROW = {"String": "string", "ID": "string", "Boolean": "bool"}
_LEAF_NUMERIC = {"Int", "Float"}


# ---------------------------------------------------------------------------------------------
# Schema loading and type plumbing
# ---------------------------------------------------------------------------------------------

def load_schema(path):
    """The saved introspection result: `{"data": {"__schema": …}}` (a raw response), `{"__schema":
    …}` (the payload), or the bare schema object. No network at ingestion time — the description
    is the unit of trust and must be locally complete, the OpenAPI adapter's URL-`$ref` rule."""
    with open(path) as f:
        doc = json.load(f)
    if isinstance(doc, dict) and "data" in doc and isinstance(doc["data"], dict):
        doc = doc["data"]
    if isinstance(doc, dict) and "__schema" in doc:
        doc = doc["__schema"]
    if not (isinstance(doc, dict) and isinstance(doc.get("types"), list)):
        raise SystemExit(f"{path}: not an introspection schema (no `types` list)")
    return doc


def type_index(schema):
    return {t["name"]: t for t in schema["types"] if t.get("name")}


def shape(tref):
    """A type reference as nested nullability: `{"nonnull": b, "list": <shape>}` for a list,
    `{"nonnull": b, "name": n, "kind": k}` for a named type."""
    nonnull = False
    if tref.get("kind") == "NON_NULL":
        nonnull = True
        tref = tref["ofType"]
    if tref.get("kind") == "LIST":
        return {"nonnull": nonnull, "list": shape(tref["ofType"])}
    return {"nonnull": nonnull, "name": tref.get("name"), "kind": tref.get("kind")}


def type_text(tref):
    """The GraphQL type syntax (`[String!]!`) for a variable declaration in the document."""
    if tref.get("kind") == "NON_NULL":
        return type_text(tref["ofType"]) + "!"
    if tref.get("kind") == "LIST":
        return "[" + type_text(tref["ofType"]) + "]"
    return tref["name"]


def named_of(sh):
    while "list" in sh:
        sh = sh["list"]
    return sh


def _pct(text):
    """RFC 3986 strict percent-encoding of SPEC-TIME literal text (the document is description
    data, fixed at generation time). Byte-for-byte the `url_encode` builtin's mapping, which
    handles the CALLER-supplied variables at run time."""
    return "".join(c if c in _UNRESERVED else "".join(f"%{b:02X}" for b in c.encode()) for c in text)


def _param_name(raw):
    name = re.sub(r"[^a-zA-Z0-9]", "_", raw)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name).lower()
    name = re.sub(r"_+", "_", name).strip("_")
    if not name or not name[0].isalpha():
        name = "p_" + name
    return name


def _intent_ext(lead, name):
    tag = f"{lead}/{_param_name(name).replace('_', '-')}"
    return [tag] if len(tag) <= 64 else []


def s_lit(s):
    return b_lit({"kind": "string", "value": s})


def curried_app(fn, *args):
    node = fn
    for a in args:
        node = b_app(node, [a])
    return node


def str_concat_chain(tokens):
    node = tokens[-1]
    for t in reversed(tokens[:-1]):
        node = curried_app(b_var("str_concat"), t, node)
    return node


def _case_bool(test, then_expr, else_expr):
    return {"kind": "case", "scrutinee": test,
            "arms": [{"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": then_expr},
                     {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": False}}, "body": else_expr}]}


def _case_just(scrutinee, bind, body):
    """`case s of { Just(x) => body; _ => None }`."""
    return {"kind": "case", "scrutinee": scrutinee,
            "arms": [{"pattern": {"kind": "variant", "tag": "Just", "payload": {"kind": "bind", "name": bind}},
                      "body": body},
                     {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}


def _case_tag(scrutinee, tag, bind, body):
    """`case s of { Tag(x) => body; _ => None }`."""
    return {"kind": "case", "scrutinee": scrutinee,
            "arms": [{"pattern": {"kind": "variant", "tag": tag, "payload": {"kind": "bind", "name": bind}},
                      "body": body},
                     {"pattern": {"kind": "wildcard"}, "body": NONE_V}]}


# ---------------------------------------------------------------------------------------------
# Selection sets: the deterministic projection of a return type
# ---------------------------------------------------------------------------------------------

def _field_selectable(f):
    """A field can be selected without values iff every argument is nullable or defaulted."""
    for a in f.get("args") or []:
        if a["type"].get("kind") == "NON_NULL" and a.get("defaultValue") is None:
            return False
    return True


def select(tindex, sh, depth, max_depth):
    """The selection for a return type shape: `(text, check)` where `text` is the selection set
    (`""` for a scalar/enum), and `check` is what the observation gate holds the value to —
    nullability at every level, list-ness, and for objects the selected leaves (each PRESENT,
    the GraphQL guarantee, and typed). Returns None where no selection exists: an object with
    no argument-free scalar leaf (an empty selection set is not a legal document), or a union
    (no common field to select — `__typename` alone would project nothing)."""
    named = named_of(sh)
    t = tindex.get(named["name"]) or {}
    kind = t.get("kind") or named["kind"]
    base = {"nonnull": sh["nonnull"]}
    if "list" in sh:
        inner = select(tindex, sh["list"], depth, max_depth)
        if inner is None:
            return None
        text, sub = inner
        return text, {**base, "list": sub}
    if kind in ("SCALAR", "ENUM"):
        chk = {**base, "kind": "scalar", "name": named["name"]}
        if kind == "ENUM":
            chk["enum"] = sorted(v["name"] for v in t.get("enumValues") or [])
        return "", chk
    if kind in ("OBJECT", "INTERFACE"):
        parts, fields = [], {}
        for f in t.get("fields") or []:
            if not _field_selectable(f):
                continue
            fsh = shape(f["type"])
            fnamed = named_of(fsh)
            ft = tindex.get(fnamed["name"]) or {}
            fkind = ft.get("kind") or fnamed["kind"]
            if fkind in ("SCALAR", "ENUM"):
                _, chk = select(tindex, fsh, depth, max_depth)
                parts.append(f["name"])
                fields[f["name"]] = chk
            elif fkind in ("OBJECT", "INTERFACE") and depth < max_depth:
                inner = select(tindex, fsh, depth + 1, max_depth)
                if inner is None:
                    continue
                text, chk = inner
                parts.append(f"{f['name']} {text}")
                fields[f["name"]] = chk
        if not parts:
            return None
        return "{ " + " ".join(parts) + " }", {**base, "kind": "object", "fields": fields}
    return None  # UNION (or an unknown kind): refuse


def _observed_conforms(v, chk, path):
    """Hold an OBSERVED value (evaluator value AST) to the declared shape the record promises:
    a non-null position is not `JNull`; a list position is a `JList` whose every element conforms;
    an object position is a `JObj` carrying EVERY selected leaf (a selected field is present in
    the response data by the GraphQL spec — absence is a protocol violation, not a value); a
    scalar leaf carries its declared constructor, an enum leaf a declared value. Custom scalars
    are unconstrained (the schema promises nothing about their wire form). Returns (ok, why)."""
    tag = v.get("tag") if isinstance(v, dict) else None
    if tag == "JNull":
        if chk.get("nonnull"):
            return False, f"`{path}` is null but declared non-null"
        return True, ""
    if "list" in chk:
        if tag != "JList":
            return False, f"`{path}` is not a list"
        for i, e in enumerate((v.get("payload") or {}).get("elems", [])):
            ok, why = _observed_conforms(e, chk["list"], f"{path}[{i}]")
            if not ok:
                return ok, why
        return True, ""
    if chk.get("kind") == "object":
        if tag != "JObj":
            return False, f"`{path}` is not an object"
        entries = {e["key"]: e["value"] for e in (v.get("payload") or {}).get("entries", [])}
        for name, sub in chk["fields"].items():
            if name not in entries:
                return False, f"selected field `{path}.{name}` absent from the response data"
            ok, why = _observed_conforms(entries[name], sub, f"{path}.{name}")
            if not ok:
                return ok, why
        return True, ""
    name = chk.get("name")
    if "enum" in chk:
        if tag != "JStr" or (v.get("payload") or {}).get("value") not in chk["enum"]:
            return False, f"`{path}` is not a declared value of enum {name}"
        return True, ""
    want = {"String": "JStr", "ID": "JStr", "Boolean": "JBool", "Int": "JNum", "Float": "JNum"}.get(name)
    if want is None:
        return True, ""  # custom scalar: unconstrained
    if tag != want:
        return False, f"`{path}` is not the declared {name}"
    if name == "Int" and (v.get("payload") or {}).get("kind") != "int":
        return False, f"`{path}` is not an integer"
    return True, ""


# ---------------------------------------------------------------------------------------------
# Record synthesis: one root field -> the pending projections
# ---------------------------------------------------------------------------------------------

def _variables_expr(params):
    """`JObj (map_put "a" (JStr a) (map_put "b" (JNum b) map_empty))` — the variables object as
    a Json VALUE; `render_json` serializes it. `json`-sorted parameters are placed raw."""
    node = b_var("map_empty")
    for gql_name, var, sort, ctor in reversed(params):
        val = b_var(var) if ctor is None else b_variant(ctor, b_var(var))
        node = curried_app(b_var("map_put"), s_lit(gql_name), val, node)
    return b_variant("JObj", node)


def _call_expr(document, params, transport, auth_header):
    """The single `http` call for a root field, by transport."""
    headers = b_var("map_empty")
    if auth_header is not None:
        headers = curried_app(b_var("map_put"), s_lit(auth_header[0]), s_lit(auth_header[1]), headers)
    if transport == "get":
        tokens = [s_lit("?query=" + _pct(document))]
        if params:
            tokens.append(s_lit("&variables="))
            tokens.append(curried_app(b_var("url_encode"),
                                      curried_app(b_var("render_json"), _variables_expr(params))))
        url = curried_app(b_var("str_concat"), b_var("base"), str_concat_chain(tokens))
        return curried_app(b_var("http"), s_lit("GET"), url, headers, s_lit("")), "net.read"
    headers = curried_app(b_var("map_put"), s_lit("Content-Type"), s_lit("application/json"), headers)
    inner = b_var("map_empty")
    if params:
        inner = curried_app(b_var("map_put"), s_lit("variables"), _variables_expr(params), inner)
    inner = curried_app(b_var("map_put"), s_lit("query"), b_variant("JStr", s_lit(document)), inner)
    body = curried_app(b_var("render_json"), b_variant("JObj", inner))
    return curried_app(b_var("http"), s_lit("POST"), b_var("base"), headers, body), "net.write"


def _projection_body(lam_params, call, field, prop=None, kind="json"):
    """`\\params -> let r = call in` status-guarded (200 — the only status GraphQL-over-HTTP
    answers a JSON document with), `parse_json r.body` -> `JObj m` -> `map_get "data" m` ->
    `JObj d` -> `map_get "<field>" d` (the whole value, Maybe Json); a typed projection goes one
    step further into the field's object and narrows the leaf by constructor."""
    data_field = curried_app(b_var("map_get"), s_lit(field), b_var("d"))
    if prop is None:
        get = data_field
    else:
        leaf = curried_app(b_var("map_get"), s_lit(prop), b_var("o"))
        if kind == "string":
            narrowed = _case_just(leaf, "w", _case_tag(b_var("w"), "JStr", "s",
                                                       {"kind": "variant", "tag": "Just", "payload": b_var("s")}))
        elif kind == "bool":
            narrowed = _case_just(leaf, "w", _case_tag(b_var("w"), "JBool", "b",
                                                       {"kind": "variant", "tag": "Just", "payload": b_var("b")}))
        else:
            narrowed = leaf
        get = _case_just(data_field, "v", _case_tag(b_var("v"), "JObj", "o", narrowed))
    on_data = _case_just(curried_app(b_var("map_get"), s_lit("data"), b_var("m")), "x",
                         _case_tag(b_var("x"), "JObj", "d", get))
    on_status = _case_just(curried_app(b_var("parse_json"), b_field(b_var("r"), "body")), "j",
                           _case_tag(b_var("j"), "JObj", "m", on_data))
    return {"kind": "lambda", "params": [{"name": p} for p in lam_params],
            "body": b_let("r", call, _case_bool(
                curried_app(b_var("eq"), b_field(b_var("r"), "status"), b_lit({"kind": "nat", "value": 200})),
                on_status, NONE_V))}


def _arg_value(sort, text):
    """An `--observe-arg` value, parsed by the parameter's sort into a value AST."""
    if sort == "string":
        return {"kind": "string", "value": text}
    if sort == "int":
        return {"kind": "int", "value": int(text)}
    if sort == "float":
        return {"kind": "float", "value": float(text)}
    if sort == "bool":
        if text not in ("true", "false"):
            raise ValueError(f"a bool argument must be `true` or `false`, got {text!r}")
        return {"kind": "bool", "value": text == "true"}
    return _json_to_value(json.loads(text))


def _json_to_value(x):
    """A Python JSON document as the Json-sum VALUE the evaluator uses (argument encoding)."""
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
        return {"kind": "variant", "tag": "JList",
                "payload": {"kind": "list", "elems": [_json_to_value(e) for e in x]}}
    if isinstance(x, dict):
        return {"kind": "variant", "tag": "JObj",
                "payload": {"kind": "map", "entries": [{"key": k, "value": _json_to_value(x[k])}
                                                       for k in sorted(x)]}}
    raise ValueError(f"not a JSON value: {x!r}")


def build_field(tindex, field, *, transport="get", auth_header=None, select_depth=SELECT_DEPTH_DEFAULT,
                observe=None):
    """One root `Query` field -> `("ok", pending, notes)` or `("skip", name, reason)`. `pending`
    is the list of projections the observation gate may materialize (the whole value first,
    then one typed projection per scalar leaf of an object result); nothing is a record yet —
    a record exists only once an observation has been held to the declared shape."""
    name = field["name"]
    notes = []
    observe = observe or {}
    # Arguments -> parameters. Required (non-null, undefaulted) arguments are the minimal
    # documented call; optional ones are omitted with a note (never a silent truncation).
    params = []  # (gql name, variable name, sort, Json ctor or None)
    required_texts = []
    var_names = {"base"}
    bound = observe.get(name) or {}
    for a in field.get("args") or []:
        required = a["type"].get("kind") == "NON_NULL" and a.get("defaultValue") is None
        if not required and _param_name(a["name"]) not in bound:
            notes.append(f"{name}: optional argument `{a['name']}` omitted (the record is the minimal "
                         "documented call)")
            continue
        if not required:
            notes.append(f"{name}: optional argument `{a['name']}` INCLUDED at the operator's binding — "
                         "the record is the minimal call widened by what the operator named")
        var = _param_name(a["name"])
        if var in var_names:
            return "skip", name, f"parameter name collision on `{var}`"
        var_names.add(var)
        ash = shape(a["type"])
        anamed = named_of(ash)
        akind = (tindex.get(anamed["name"]) or {}).get("kind") or anamed["kind"]
        if "list" not in ash and anamed["name"] in _SCALAR_PARAMS:
            sort, _, ctor = _SCALAR_PARAMS[anamed["name"]]
        elif "list" not in ash and akind == "ENUM":
            sort, ctor = "string", "JStr"
            notes.append(f"{name}: enum argument `{a['name']}` rides as a string (a value outside the "
                         "declared set is the service's to refuse)")
        else:
            sort, ctor = "json", None
            notes.append(f"{name}: argument `{a['name']}: {type_text(a['type'])}` is a `Json` parameter "
                         "(input object / list / custom scalar — caller data the adapter cannot narrow)")
        params.append((a["name"], var, sort, ctor))
        required_texts.append(type_text(a["type"]))
    # The selection set: the deterministic projection of the return type.
    rsh = shape(field["type"])
    sel = select(tindex, rsh, 1, select_depth)
    if sel is None:
        rnamed = named_of(rsh)
        rkind = (tindex.get(rnamed["name"]) or {}).get("kind") or rnamed["kind"]
        return "skip", name, (f"return type `{type_text(field['type'])}` ({rkind}) has no selectable "
                              "argument-free scalar leaf — no legal selection set")
    sel_text, check = sel
    # The document: a spec-time literal. Variables carry every caller value.
    if params:
        decl = ", ".join(f"${var}: {ttext}" for (_, var, _, _), ttext in zip(params, required_texts))
        args_text = "(" + ", ".join(f"{gql}: ${var}" for gql, var, _, _ in params) + ")"
        document = f"query Q({decl}) {{ {name}{args_text} {sel_text} }}"
    else:
        document = f"query Q {{ {name} {sel_text} }}"
    document = re.sub(r"\s+", " ", document).strip()
    call, effect = _call_expr(document, params, transport, auth_header)
    lam_params = ["base"] + [var for _, var, _, _ in params]
    param_types = [STRING] + [{"string": STRING, "int": INT, "float": FLOAT, "bool": BOOL, "json": JSON_T}[sort]
                              for _, _, sort, _ in params]
    # Observation arguments: an argument-free field is constructible; otherwise every parameter
    # must be bound by the operator.
    unknown = sorted(set(bound) - {var for _, var, _, _ in params})
    if unknown:
        return "skip", name, f"--observe-arg names parameter(s) {unknown} that `{name}` does not take"
    args = None
    if params:
        missing = [var for _, var, _, _ in params if var not in bound]
        if not missing:
            try:
                args = [{"kind": "string", "value": "{{base}}"}] + [_arg_value(sort, bound[var])
                                                                     for _, var, sort, _ in params]
            except ValueError as e:
                return "skip", name, f"--observe-arg value unusable: {e}"
        else:
            notes.append(f"{name}: needs --observe-arg for {missing} to observe (the schema cannot name "
                         "the server state a required argument selects)")
    else:
        args = [{"kind": "string", "value": "{{base}}"}]
    base_tags = ["io", "io/network/http", "query/lookup", "parse"]
    pending = [{
        "name": name, "hint": _param_name(name), "field": name, "prop": None, "required": False,
        "type_ast": {"kind": "fn", "params": param_types, "result": MAYBE_JSON},
        "body_ast": _projection_body(lam_params, call, name),
        "args": args, "effect": effect, "check": check, "document": document, "transport": transport,
        "intent": base_tags + _intent_ext("query/lookup", name),
    }]
    # Typed projections: the scalar leaves of a (non-list) object result.
    if "list" not in check and check.get("kind") == "object":
        for prop, chk in check["fields"].items():
            if chk.get("kind") != "scalar":
                continue  # a nested object leaf rides inside the whole-value projection
            if "enum" in chk:
                kind, rtype = "string", MAYBE_STRING
            elif chk["name"] in _LEAF_NARROW:
                kind = _LEAF_NARROW[chk["name"]]
                rtype = MAYBE_STRING if kind == "string" else MAYBE_BOOL
            elif chk["name"] in _LEAF_NUMERIC:
                notes.append(f"{name}: leaf `{prop}: {chk['name']}` not projected (JNum carries int or "
                             "float; a typed numeric promise cannot be narrowed soundly by pattern)")
                continue
            else:
                kind, rtype = "json", MAYBE_JSON  # custom scalar: as data
            pname = f"{name}{prop[:1].upper()}{prop[1:]}"
            pending.append({
                "name": pname, "hint": _param_name(pname), "field": name, "prop": prop,
                "required": bool(chk.get("nonnull")) and bool(check.get("nonnull")),
                "type_ast": {"kind": "fn", "params": param_types, "result": rtype},
                "body_ast": _projection_body(lam_params, call, name, prop, kind),
                "args": args, "effect": effect, "check": chk, "document": document, "transport": transport,
                "intent": base_tags + _intent_ext("parse", pname),
            })
    return "ok", pending, notes


def walk(schema, *, transport="get", auth_header=None, select_depth=SELECT_DEPTH_DEFAULT, observe=None):
    """Every root field of the schema: Query fields compile; Mutation/Subscription fields refuse
    (read-only by rule). Returns (pending, skipped, notes, report)."""
    tindex = type_index(schema)
    observe = observe or {}
    pending, skipped, notes = [], [], []
    report = {"query_fields": 0, "compiled": 0, "refused": 0, "mutation_fields": 0,
              "subscription_fields": 0, "projections": 0}
    qname = (schema.get("queryType") or {}).get("name")
    for f in (tindex.get(qname) or {}).get("fields") or [] if qname else []:
        report["query_fields"] += 1
        st, a, b = build_field(tindex, f, transport=transport, auth_header=auth_header,
                               select_depth=select_depth, observe=observe)
        if st == "ok":
            report["compiled"] += 1
            report["projections"] += len(a)
            pending.extend(a)
            notes.extend(b)
        else:
            report["refused"] += 1
            skipped.append((a, b))
    for root, key in (("mutationType", "mutation_fields"), ("subscriptionType", "subscription_fields")):
        rname = (schema.get(root) or {}).get("name")
        fields = (tindex.get(rname) or {}).get("fields") or [] if rname else []
        report[key] = len(fields)
        for f in fields:
            skipped.append((f["name"], f"{root[:-4]} root field: read-only by rule (an observation must "
                                      "not create state during ingestion; no worked example is "
                                      "spec-derivable)"))
    unheeded = sorted(set(observe) - {f["name"] for f in (tindex.get(qname) or {}).get("fields") or []})
    return pending, skipped, notes, report, unheeded


# ---------------------------------------------------------------------------------------------
# The observation gate (the GW12 shape, exactly as the OpenAPI adapter runs it)
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


def certify(record_path, body_path, out_dir):
    r = subprocess.run([_VALIDATOR, "certify", record_path, "--body", body_path, "--records", out_dir],
                       capture_output=True, text=True)
    return r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip()


def _traced_call(trace_path):
    try:
        trace = json.load(open(trace_path))
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


def materialize(p, out_dir, base_url, secrets, blob_threshold=BLOB_THRESHOLD_DEFAULT, replay_from=None):
    """Run the projection ONCE against the live service (grant = the record's effect, secrets
    supplied), hold the observation to the declared shape, and only then mint the record with
    the observation as its trace-attached worked example. Returns (ok, record_or_message).

    `replay_from` = the trace a SIBLING projection already recorded for the byte-identical
    request (same document, same arguments): the body then runs by `eval --replay` — no live
    call, the same observation, the same `trc_` address — so a root field costs the service one
    request however many projections it licenses (the AniList 429 finding: 142 projections were
    142 requests, and the service refused the flood). The evidence is still per-record: each
    record's example carries the trace it was judged on."""
    fbase = sanitize_hint(p["name"])
    bp = os.path.join(out_dir, f"body-{fbase}.json")
    json.dump(p["body_ast"], open(bp, "w"), indent=2)
    args = [dict(a) for a in p["args"]]
    args[0] = {"kind": "string", "value": base_url}
    argfiles = []
    for j, a in enumerate(args):
        ap = os.path.join(out_dir, f".arg-{fbase}-{j}.json")
        json.dump(a, open(ap, "w"))
        argfiles.append(ap)
    trace_path = os.path.join(out_dir, f"trace-{fbase}-0.json")
    cmd = [_VALIDATOR, "eval", bp]
    for ap in argfiles:
        cmd += ["--arg", ap]
    cmd += ["--grant", p["effect"]]
    for n, v in secrets:
        cmd += ["--secret", f"{n}={v}"]
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
        return False, f"live observation failed: {(r.stderr or '').strip()}"
    got = json.loads(r.stdout)
    # Each projection is judged on ITS OWN call's evidence (gcp finding 8): the trace holds the
    # real status and body; a failed call and an absent field collapse to the same `None`.
    t_status, t_body = _traced_call(trace_path)
    if t_status != 200:
        return False, (f"the call answered {t_status}, not 200 — no document was obtained, so nothing "
                       "can be observed")
    try:
        envelope = json.loads(t_body if t_body is not None else "")
    except (ValueError, TypeError):
        return False, "the 200 response body is not JSON — no document was obtained"
    if not isinstance(envelope, dict) or "data" not in envelope:
        return False, "the response carries no `data` — the request was not executed (errors: "
        + json.dumps(envelope.get("errors") if isinstance(envelope, dict) else envelope)[:300] + ")"
    if envelope.get("errors"):
        return False, ("the response carries `errors` alongside `data` — a partial document is not the "
                       "declared one: " + json.dumps(envelope["errors"])[:300])
    is_none = isinstance(got, dict) and got.get("kind") == "variant" and got.get("tag") == "None"
    if p["prop"] is None:
        if is_none:
            return False, f"`data.{p['field']}` absent from an executed response — a protocol violation"
        ok, why = _observed_conforms(got.get("payload"), p["check"], f"data.{p['field']}")
        if not ok:
            return False, f"observed value violates the declared type: {why}"
    elif p["required"] and is_none:
        return False, (f"non-null leaf `{p['field']}.{p['prop']}` absent or mistyped in the response — "
                       "the schema's promise does not hold")
    trc = subprocess.run([_VALIDATOR, "hash", trace_path], capture_output=True, text=True).stdout.strip()
    if not trc.startswith("trc_"):
        return False, f"trace did not hash to a trc_… address: {trc!r}"
    example = {"args": args, "result": got, "trace": trc}
    blobify_example(example, out_dir, blob_threshold)
    record = build_v2_record(name=p["name"], type_ast=p["type_ast"], examples=[example],
                             body_text=p["body_ast"], module_name=None, extra_hints=[p["hint"]],
                             effects=[p["effect"]], terminates="always", intent_tags=p["intent"],
                             complexity="O(n)")
    rp = os.path.join(out_dir, f"{fbase}.v0.2.json")
    json.dump(record, open(rp, "w"), indent=2)
    return True, record


def verify_examples(record_path, out_dir):
    """Replay with NO secrets and no grants beyond the record's own: the offline check any commons
    consumer can perform."""
    r = subprocess.run([_VALIDATOR, "run", record_path, "--records", out_dir], capture_output=True, text=True)
    return r.returncode == 0, (r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip())


# ---------------------------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------------------------

def parse_observe_args(items):
    """`field.arg=value` -> {field: {arg_var: value}}. The FIRST dot splits field from argument
    (GraphQL names carry no dots)."""
    out = {}
    for item in items or []:
        if "=" not in item or "." not in item.split("=", 1)[0]:
            raise SystemExit(f"--observe-arg expects <field>.<arg>=<value>, got {item!r}")
        key, value = item.split("=", 1)
        field, arg = key.split(".", 1)
        out.setdefault(field, {})[_param_name(arg)] = value
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__.split("\n\n")[0],
        epilog="Needs the nl-validator binary: the sibling tooling/validator build, else the one quickstart.sh "
               "fetched into .quickstart/, else set NL_VALIDATOR=/path/to/nl-validator. Output: <name>.v0.2.json records, "
               "body-<name>.json bodies, trace-<name>-<i>.json traces, where <name> is the field name LOWERCASED "
               "(countryCapital -> countrycapital.v0.2.json); a list-valued argument binds as JSON text: "
               "--observe-arg 'charactersByIds.ids=[\"1\",\"2\"]'; certify one by hand with "
               "`nl-validator certify <record> --body <body> --records <out>`; publish with "
               "tooling/commons-node/publish_records.py <out>.")
    ap.add_argument("schema", help="a saved introspection result (JSON)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--transport", choices=("get", "post"), default="get",
                    help="GraphQL-over-HTTP transport (a server property the schema does not declare)")
    ap.add_argument("--select-depth", type=int, default=SELECT_DEPTH_DEFAULT,
                    help="how deep object-valued leaves are selected (1 = scalars of the result only)")
    ap.add_argument("--auth-bearer", default=None, metavar="NAME",
                    help="send `Authorization: Bearer {{secret:NAME}}` (introspection declares no auth)")
    ap.add_argument("--verify-against", default=None, metavar="URL", help="the endpoint URL: the observation gate")
    ap.add_argument("--token", default="test-token", help="the live-gate credential for --auth-bearer")
    ap.add_argument("--observe-arg", action="append", default=[], metavar="FIELD.ARG=VALUE")
    ap.add_argument("--blob-threshold", type=int, default=BLOB_THRESHOLD_DEFAULT)
    ap.add_argument("--pace", type=float, default=0.0, metavar="SECONDS",
                    help="minimum spacing between LIVE calls (a public service's rate limit is the operator's to respect)")
    a = ap.parse_args(argv)

    schema = load_schema(a.schema)
    observe = parse_observe_args(a.observe_arg)
    if observe and not a.verify_against:
        ap.error("--observe-arg requires --verify-against (an observation needs a service)")
    auth_header = ("Authorization", "Bearer {{secret:%s}}" % a.auth_bearer) if a.auth_bearer else None
    pending, skipped, notes, report, unheeded = walk(schema, transport=a.transport, auth_header=auth_header,
                                                     select_depth=a.select_depth, observe=observe)
    for name, why in skipped:
        print(f"skip {name}: {why}")
    for n in notes:
        print(f"note {n}")
    if unheeded:
        print(f"refuse: --observe-arg binds field(s) {unheeded} the schema's Query type does not declare")
        sys.exit(2)
    for k, v in report.items():
        print(f"report {k}={v}")
    os.makedirs(a.out, exist_ok=True)
    if not a.verify_against:
        for p in pending:
            print(f"licensed {p['name']} : {len(p['type_ast']['params'])} params -> "
                  f"{'Maybe Json' if p['type_ast']['result'] == MAYBE_JSON else 'Maybe ' + p['type_ast']['result']['variants'][0]['type']['name']}"
                  f"  [{p['effect']}]  {'observable' if p['args'] else 'needs --observe-arg'}")
        print(f"summary: {len(pending)} projections licensed, 0 materialized (no --verify-against — a schema "
              "licenses shapes; only an observation supplies a value)")
        return
    secrets = [(a.auth_bearer, a.token)] if a.auth_bearer else []
    ok_all = True
    made = 0
    live_calls = 0
    observed = {}  # (document, args) -> the trace a sibling recorded; the same request is never re-issued
    for p in pending:
        if p["args"] is None:
            print(f"{p['name']}: not observed (needs --observe-arg)")
            continue
        key = (p["document"], json.dumps(p["args"], sort_keys=True))
        replay_from = observed.get(key)
        if isinstance(replay_from, tuple):  # a sibling's request already failed: inherit the verdict, no call
            print(f"{p['name']}: observation-gate=FAIL (same request as {replay_from[0]}) {replay_from[1]}")
            ok_all = False
            continue
        if replay_from is None:
            if live_calls and a.pace > 0:
                import time
                time.sleep(a.pace)
            live_calls += 1
        ok, res = materialize(p, a.out, a.verify_against, secrets, a.blob_threshold, replay_from=replay_from)
        if replay_from is None:
            observed[key] = (os.path.join(a.out, f"trace-{sanitize_hint(p['name'])}-0.json") if ok
                             else (p["name"], res))
        if not ok:
            print(f"{p['name']}: observation-gate=FAIL {res}")
            ok_all = False
            continue
        made += 1
        rp = os.path.join(a.out, f"{sanitize_hint(p['name'])}.v0.2.json")
        bp = os.path.join(a.out, f"body-{sanitize_hint(p['name'])}.json")
        c_ok, c_msg = certify(rp, bp, a.out)
        v_ok, v_msg = verify_examples(rp, a.out)
        ex = res["examples"][0]
        by_addr = f"BY-ADDRESS({ex['result_blob']['bytes']} bytes)" if "result_blob" in ex else "inline"
        print(f"{p['name']}: observation-gate=OK({'replayed' if replay_from else 'live'}) "
              f"certify={'OK' if c_ok else 'FAIL'} replay={'OK' if v_ok else 'FAIL'} example={by_addr} "
              f"trace={ex['trace'][:16]}… {res['hash'][:16]}… -> {sanitize_hint(p['name'])}.v0.2.json")
        if not (c_ok and v_ok):
            print(f"  {c_msg if not c_ok else v_msg}")
            ok_all = False
    print(f"summary: {made} records materialized of {len(pending)} licensed from {live_calls} live calls; "
          f"{'all certified + replayed' if ok_all else 'FAILURES above'}")
    sys.exit(0 if ok_all else 1)


if __name__ == "__main__":
    main()
