#!/usr/bin/env python3
"""Generate verified Nova Lingua records from an OpenAPI 3 description.

A machine-readable API description is an *ingestion source*: each operation (a path × method) has
a well-defined surface — base URL, HTTP verb, path parameters, request body, auth scheme — which
is exactly the semantic content of a client function. So one operation maps to one Nova Lingua
record over the general `http` builtin (spec/expressiveness.md GW6), with no hand-authoring:

    operationId  -> record name_hints
    verb         -> the `http` method literal, and the effect (net.read for GET/HEAD, net.write else)
    server url    -> the `base` parameter (records stay host-portable; the server url is the example base)
    path template -> a `str_concat` URL builder over the base and the path-parameter variables
    path params  -> string parameters, in template order
    query params -> required ones become parameters too: a string value rides through the
                    `url_encode` builtin (raw str_concat of caller data into a URL is UNSOUND —
                    a space or `&` changes the request; GW10 pulled url_encode for exactly this),
                    an `integer`-schema value through `to_string` (digits are unreserved);
                    OPTIONAL query/header params are omitted with a printed note — the record is
                    the minimal documented call, never a silent truncation
    header params -> required ones become string parameters, `map_put` into the header map
    requestBody  -> a `body` string parameter (omitted for bodyless verbs); a MULTIPART-only
                    body compiles to a deterministic form: the boundary is a spec-time constant
                    riding in the Content-Type literal, part names are description data, and each
                    REQUIRED string part (incl. format: binary) becomes a caller parameter —
                    framing is literal, only part values are caller data (the url_encode split).
                    Optional parts are omitted with a note. Refused honestly: a multipart body
                    with no declared part properties, no required parts, or a non-string part
    security     -> `http`/`bearer` -> an `Authorization: Bearer {{secret:NAME}}` header;
                    `apiKey` in `header` -> a `<name>: {{secret:NAME}}` header. apiKey in
                    query/cookie is REFUSED: a secret placeholder substitutes only inside a
                    HEADER value at the effect boundary (GW6) — in a query string it would enter
                    the URL, hence the record and the trace. HTTP basic (needs base64 — no
                    builtin) and oauth2/openIdConnect flows are refused too
    $ref         -> local `#/...` references (parameters, requestBodies, responses, security
                    schemes, path-item-level shared parameters) are resolved, and so are
                    RELATIVE-FILE references (`schemas.json#/...`) against the spec's own
                    directory — the referenced document's refs resolve against *that* document
                    and the imported subtree comes back fully inlined. URL references (no network
                    at ingestion time), absolute paths, paths escaping the spec's directory, and
                    dangling/cyclic references refuse the operation honestly
    responses    -> the documented status the worked example asserts; a 2xx response that
                    documents an application/json EXAMPLE additionally yields a body-projection
                    record `<opId>Body : … -> Maybe Json` (`parse_json` over the response body —
                    field access then composes in-language via the certified json_get/json_path
                    commons records, principle 4). Emitted only when a deterministic success
                    example is constructible from the spec alone (a GET with no path parameters);
                    anything else gets a printed note, never a silent guess.
                    A 2xx response that declares only a SCHEMA (no example — how real-world
                    descriptions overwhelmingly document responses) yields SCHEMA-DERIVED
                    projections: `<opId>Body : … -> Maybe Json` plus one typed field projection
                    per declared property that narrows soundly (`string` -> Maybe string via
                    JStr, `boolean` -> Maybe bool via JBool, object/array/untyped -> Maybe Json;
                    numeric properties are noted, not projected — JNum carries int OR float, so
                    a typed numeric promise cannot be narrowed by pattern alone). A schema
                    promises SHAPE, not a value, so these records exist only through the live
                    observation gate (--verify-against): one execution supplies the worked
                    example (trace-attached, offline-replayable), and the observed document is
                    HELD TO the declared shape — required properties present, declared types
                    match — so a description the service does not honor refuses to publish.
    resp headers -> a header the documented response DECLARES with an example (Location being
                    the canonical case — server-assigned identity, redirect targets) yields a
                    header-projection record `<opId><Header> : … -> Maybe string` over the
                    header-preserving `http_full` (GW16): the call bound once, status-guarded to
                    the documented response, `map_get` of the lowercase name. A declared header
                    without a documented example is noted, never guessed.

Each generated status record RETURNS the response `.status` (an int) — the always-deterministic
part of a response. Body-projection records carry the payload as data; their asserts under grants
are `observed` claims (trace-conditioned, replay-verifiable — spec/trace.schema.json), which is
what makes a third-party-checkable claim about a response body possible at all.
Every record is gated through `nl-validator certify` (typecheck / effects / termination /
complexity); with `--verify-against <base-url>` the worked examples are additionally RUN against a
live service (a fake or a real one), the "gate = examples vs an emulator" step — so a generated
record is verified-by-default exactly like a hand-authored one.

    python3 openapi_ingest.py <spec.json> --out <dir> [--secret-name api_token]
                              [--verify-against http://127.0.0.1:8878 --token test-token]

Vendor-neutral by construction: OpenAPI is the input dialect; nothing here is specific to any
provider. Reuses ingest-common (BLAKE3+JCS core, body-AST builders).
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

from nl_body import b_app, b_field, b_let, b_lit, b_var  # noqa: E402
from nl_core import build_v2_record, canonicalize, expr_address, sanitize_hint  # noqa: E402

STRING = {"kind": "builtin", "name": "string"}
INT = {"kind": "builtin", "name": "int"}
BOOL = {"kind": "builtin", "name": "bool"}
JSON_T = {"kind": "builtin", "name": "Json"}
# The structural sum {Just Json, None} — how a Maybe result is declared (the json_get precedent).
MAYBE_JSON = {"kind": "sum", "variants": [{"tag": "Just", "type": JSON_T}, {"tag": "None"}]}
MAYBE_STRING = {"kind": "sum", "variants": [{"tag": "Just", "type": STRING}, {"tag": "None"}]}
MAYBE_BOOL = {"kind": "sum", "variants": [{"tag": "Just", "type": BOOL}, {"tag": "None"}]}
NONE_V = {"kind": "variant", "tag": "None"}
_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator")
)
_READ_VERBS = {"GET", "HEAD"}


def s_lit(text):
    return b_lit({"kind": "string", "value": text})


# Relative-file $ref support: the directory of the top-level spec (set by `load_spec`) and a cache
# of referenced documents. `None` base = no file context (an in-memory spec), so file refs refuse.
_BASE_DIR = None
_DOC_CACHE = {}


def load_spec(path):
    """Load the top-level spec and remember its directory as the base for RELATIVE file $refs.
    Use this instead of a bare json.load so `other.json#/...` references resolve."""
    global _BASE_DIR
    _BASE_DIR = os.path.dirname(os.path.abspath(path))
    _DOC_CACHE.clear()
    return json.load(open(path))


def _resolve_pointer(doc, frag):
    """A JSON Pointer fragment (`/components/...`, RFC 6901 `~0`/`~1` escaping) against `doc`.
    An empty fragment is the whole document. Returns None when dangling."""
    if frag in ("", "/"):
        return doc
    cur = doc
    for raw in frag.lstrip("/").split("/"):
        key = raw.replace("~1", "/").replace("~0", "~")
        if not isinstance(cur, dict) or key not in cur:
            return None
        cur = cur[key]
    return cur


def _external_deref(ref, base_dir, depth):
    """Resolve a RELATIVE-FILE $ref (`other.json`, `./sub/x.json#/Foo`) against `base_dir` and
    return a FULLY-INLINED snapshot of the target — every $ref inside the referenced document is
    resolved against *that* document (per JSON Reference semantics) before anything is returned,
    so callers never hold a node whose refs belong to a different file. Refused (None): URL refs
    (no network at ingestion time — the description must be locally complete), absolute paths, and
    any path escaping the top-level spec's directory (the description is the unit of trust; it
    does not get to read the rest of the filesystem)."""
    if _BASE_DIR is None or depth > 32:
        return None
    file_part, _, frag = ref.partition("#")
    if "://" in file_part or file_part.startswith(("http:", "https:", "//", "/")):
        return None
    path = os.path.normpath(os.path.join(base_dir, file_part))
    root = os.path.realpath(_BASE_DIR)
    real = os.path.realpath(path)
    if real != root and not real.startswith(root + os.sep):
        return None
    if path not in _DOC_CACHE:
        try:
            with open(path) as fh:
                _DOC_CACHE[path] = json.load(fh)
        except (OSError, ValueError):
            return None
    doc = _DOC_CACHE[path]
    node = _resolve_pointer(doc, frag)
    return _inline(doc, os.path.dirname(path), node, depth)


def _inline(doc, doc_dir, node, depth):
    """Deep-copy `node`, resolving every $ref it contains: local refs against `doc` (the document
    the node came from), file refs against `doc_dir` (a nested file ref is relative to its
    CONTAINING document). Any dangling/cyclic/refused ref poisons the whole snapshot to None —
    the operation then refuses honestly rather than shipping a half-resolved description."""
    if depth > 32 or node is None:
        return None
    while isinstance(node, dict) and "$ref" in node:
        ref = node["$ref"]
        if not isinstance(ref, str):
            return None
        if ref.startswith("#"):
            node = _resolve_pointer(doc, ref[1:])
            depth += 1
            if node is None:
                return None
        else:
            return _external_deref(ref, doc_dir, depth + 1)
    if isinstance(node, dict):
        out = {}
        for k, v in node.items():
            if isinstance(v, (dict, list)):
                iv = _inline(doc, doc_dir, v, depth + 1)
                if iv is None:
                    return None
                out[k] = iv
            else:
                out[k] = v
        return out
    if isinstance(node, list):
        out = []
        for v in node:
            if isinstance(v, (dict, list)):
                iv = _inline(doc, doc_dir, v, depth + 1)
                if iv is None:
                    return None
                out.append(iv)
            else:
                out.append(v)
        return out
    return node


def deref(spec, node, _depth=0):
    """Resolve a $ref chain against the spec document: a local `#/...` reference (JSON Pointer,
    RFC 6901 `~0`/`~1` escaping) walks the spec in place; a RELATIVE-FILE reference
    (`other.json#/...`) resolves against the top-level spec's directory and returns a
    fully-inlined snapshot of the referenced subtree (see `_external_deref`). Returns None for a
    URL, absolute-path, directory-escaping, dangling, or cyclic reference — the caller refuses
    the operation honestly rather than guessing."""
    while isinstance(node, dict) and "$ref" in node:
        if _depth > 32:
            return None
        ref = node["$ref"]
        if not isinstance(ref, str):
            return None
        if not ref.startswith("#/"):
            return _external_deref(ref, _BASE_DIR, _depth) if _BASE_DIR else None
        cur = spec
        for raw in ref[2:].split("/"):
            key = raw.replace("~1", "/").replace("~0", "~")
            if not isinstance(cur, dict) or key not in cur:
                return None
            cur = cur[key]
        node = cur
        _depth += 1
    return node


_UNRESERVED = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")


def _pct(text):
    """RFC 3986 strict percent-encoding of SPEC-TIME literal text (a query-parameter NAME is
    description data, fixed at generation time). Byte-for-byte the `url_encode` builtin's mapping,
    which handles the CALLER-supplied values at run time."""
    return "".join(
        c if c in _UNRESERVED else "".join(f"%{b:02X}" for b in c.encode()) for c in text
    )


def curried_app(fn, *args):
    """A curried application spine `((fn a) b) c …` — the shape the surface parser emits, so a
    generated body hashes identically to the same body written by hand."""
    node = fn
    for a in args:
        node = b_app(node, [a])
    return node


def str_concat_chain(tokens):
    """Right-fold `str_concat` over `tokens` (body-expr nodes): `str_concat t0 (str_concat t1 …)`.
    A single token is itself. Matches the hand-authored URL builders."""
    node = tokens[-1]
    for t in reversed(tokens[:-1]):
        node = curried_app(b_var("str_concat"), t, node)
    return node


def url_builder(base_var, path_template):
    """`str_concat base <path>` where <path> is the template with `{param}` refs replaced by their
    variables and literal runs kept as string literals."""
    tokens = []
    for part in re.split(r"(\{[^}]+\})", path_template):
        if not part:
            continue
        m = re.fullmatch(r"\{([^}]+)\}", part)
        tokens.append(b_var(_param_name(m.group(1))) if m else s_lit(part))
    if not tokens:  # a path that is only "/"— unusual, but keep it total
        return base_var
    return curried_app(b_var("str_concat"), base_var, str_concat_chain(tokens))


def _intent_ext(lead, name):
    """The extending discovery tag `<lead>/<name>` in the tag grammar (hyphenated lowercase
    segments, 64 chars) — or nothing when the name would overflow the bound: an omission,
    never a truncation (a truncated tag could collide with a different operation's)."""
    tag = f"{lead}/{_param_name(name).replace('_', '-')}"
    return [tag] if len(tag) <= 64 else []


def _param_name(raw):
    """A path/operation identifier down to a valid lowercase body variable."""
    name = re.sub(r"[^a-zA-Z0-9]", "_", raw)
    name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name).lower()
    name = re.sub(r"_+", "_", name).strip("_")
    if not name or not name[0].isalpha():
        name = "p_" + name
    return name


def resolve_auth(spec, op, global_security, secret_override):
    """The operation's effective auth as a header, or a refusal.

    Returns ("none", None), ("header", (header_name, header_value)), or ("refuse", reason).
    Operation-level `security: []` disables auth; absent inherits the global requirement. The
    secret placeholder name defaults to the security-scheme key (one `--secret NAME=...` per
    scheme at run time), overridable with --secret-name."""
    sec = op.get("security", global_security)
    if not sec or not sec[0]:
        return ("none", None)
    scheme_key = next(iter(sec[0]))
    schemes = (spec.get("components") or {}).get("securitySchemes") or {}
    scheme = deref(spec, schemes.get(scheme_key))
    if not isinstance(scheme, dict):
        return ("refuse", f"security scheme `{scheme_key}` unresolvable ($ref external or dangling)")
    name = secret_override or scheme_key
    kind = scheme.get("type")
    if kind == "http" and scheme.get("scheme") == "bearer":
        return ("header", ("Authorization", f"Bearer {{{{secret:{name}}}}}"))
    if kind == "apiKey" and scheme.get("in") == "header":
        return ("header", (scheme.get("name", "X-Api-Key"), f"{{{{secret:{name}}}}}"))
    if kind == "apiKey":
        return ("refuse",
                f"apiKey in `{scheme.get('in')}` — a secret placeholder substitutes only inside a "
                "header value at the effect boundary; in a query string it would enter the URL, "
                "the record, and the trace")
    if kind == "http":
        return ("refuse", f"http `{scheme.get('scheme')}` auth — needs an encoding the language "
                          "does not have (no base64 builtin)")
    if kind == "oauth2":
        # GW13: the CLIENT-CREDENTIALS flow is pure machine-to-machine — the one OAuth2 flow the
        # effect-boundary credentials doctrine can carry. The record's header names the identity
        # SYMBOLICALLY ({{oauth:NAME}}); the operator supplies token_url|client_id|client_secret
        # at run time (`--oauth`), the token is fetched inside the live effect and never enters
        # the record or the trace. Interactive flows (a user, a browser, a redirect) refuse.
        flows = scheme.get("flows") or {}
        if "clientCredentials" in flows:
            return ("header", ("Authorization", f"Bearer {{{{oauth:{name}}}}}"))
        other = ", ".join(sorted(flows)) or "none declared"
        return ("refuse", f"oauth2 without a clientCredentials flow ({other}) — interactive flows "
                          "need a principal the effect boundary cannot supply")
    return ("refuse", f"`{kind}` security — flow-based auth is out of the description subset")


def _schema_type(spec, param):
    schema = deref(spec, param.get("schema") or {})
    return (schema or {}).get("type")


def _json_to_value(x):
    """Encode a parsed JSON document as the Json-sum VALUE the evaluator's parse_json produces —
    the expected-result encoding for a body-projection example. Returns None for a document we
    refuse to promise (non-integer numbers: their canonical rendering is a float-equality
    promise the description's author never made)."""
    if x is None:
        return {"kind": "variant", "tag": "JNull"}
    if isinstance(x, bool):
        return {"kind": "variant", "tag": "JBool", "payload": {"kind": "bool", "value": x}}
    if isinstance(x, int):
        return {"kind": "variant", "tag": "JNum", "payload": {"kind": "int", "value": x}}
    if isinstance(x, float):
        return None
    if isinstance(x, str):
        return {"kind": "variant", "tag": "JStr", "payload": {"kind": "string", "value": x}}
    if isinstance(x, list):
        elems = [_json_to_value(e) for e in x]
        if any(e is None for e in elems):
            return None
        return {"kind": "variant", "tag": "JList",
                "payload": {"kind": "list", "elems": elems}}
    if isinstance(x, dict):
        entries = []
        for k in sorted(x):  # canonical map form: unique keys in code-point order
            v = _json_to_value(x[k])
            if v is None:
                return None
            entries.append({"key": k, "value": v})
        return {"kind": "variant", "tag": "JObj",
                "payload": {"kind": "map", "entries": entries}}
    return None


def _case_bool(test, then_expr, else_expr):
    """`case test of { true => then; false => else }` with LITERAL bool patterns — the exact shape
    the surface parser emits for a hand-written two-way branch, so a generated body can hash (or
    α-normalize) identically to its hand-authored twin. (`b_if`'s wildcard else arm is a different
    AST.)"""
    return {
        "kind": "case",
        "scrutinee": test,
        "arms": [
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": then_expr},
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": False}}, "body": else_expr},
        ],
    }


def _response_header_examples(spec, op, want):
    """The documented response HEADERS of the status the worked example asserts, split into
    (projectable, unprojectable): projectable = [(name, example_string)] — headers whose
    documented `example` (or schema.example) is a string, spec-time knowledge a projection
    record can promise; unprojectable = [name] — declared headers with no documented example
    (noted, never guessed). GW16 (spec/expressiveness.md): the description-layer counterpart
    of the GW14 header pull."""
    resp = deref(spec, (op.get("responses") or {}).get(str(want)))
    if not isinstance(resp, dict):
        return [], []
    projectable, unprojectable = [], []
    for name, header in sorted((resp.get("headers") or {}).items()):
        header = deref(spec, header)
        if not isinstance(header, dict):
            unprojectable.append(name)
            continue
        example = header.get("example")
        if example is None and isinstance(header.get("schema"), dict):
            example = header["schema"].get("example")
        if isinstance(example, str):
            projectable.append((name, example))
        else:
            unprojectable.append(name)
    return projectable, unprojectable


def _json_media(content):
    """The JSON media entry of a response `content` map: `application/json`, or any subtype with
    the RFC 6839 `+json` structured-syntax suffix (`application/ld+json`, `geo+json`, `hal+json`
    — the NWS finding: production APIs overwhelmingly serve JSON under suffixed types, and the
    suffix IS the parses-as-JSON promise `parse_json` needs). Parameters (`;charset=…`) are
    tolerated; non-JSON types answer None."""
    if not isinstance(content, dict):
        return None
    for ctype, media in content.items():
        bare = ctype.split(";")[0].strip().lower()
        if bare == "application/json" or bare.split("/")[-1].endswith("+json"):
            return media if isinstance(media, dict) else {}
    return None


def _response_json_example(spec, op):
    """The documented JSON EXAMPLE of the operation's first 2xx response (see `_json_media` for
    which content types count), as (status_code, parsed_example), or None. This is spec-time
    data — the one thing that makes a runnable worked example for a body projection possible
    without guessing."""
    responses = op.get("responses", {})
    for code in sorted(c for c in responses if c.isdigit() and 200 <= int(c) < 300):
        resp = deref(spec, responses[code])
        if not isinstance(resp, dict):
            continue
        media = _json_media(resp.get("content")) or {}
        example = media.get("example")
        if example is None and isinstance(media.get("schema"), dict):
            example = media["schema"].get("example")
        if example is not None:
            return int(code), example
    return None


def _response_json_schema(spec, op):
    """The declared JSON SCHEMA of the operation's first 2xx response (any `+json` content type
    — see `_json_media`), as (status_code, resolved_schema), or None. Consulted only when no
    documented example exists — real-world descriptions overwhelmingly declare schemas, not
    examples (the Frankfurter finding, spec/expressiveness.md). A schema promises SHAPE, not a
    value: it licenses a projection record, but the record's worked example must come from a
    live observation."""
    responses = op.get("responses", {})
    for code in sorted(c for c in responses if c.isdigit() and 200 <= int(c) < 300):
        resp = deref(spec, responses[code])
        if not isinstance(resp, dict):
            continue
        media = _json_media(resp.get("content")) or {}
        schema = deref(spec, media.get("schema")) if isinstance(media.get("schema"), dict) else None
        if isinstance(schema, dict):
            return int(code), schema
    return None


def _schema_object_fields(spec, schema):
    """Split an object schema's declared properties into (projectable, skipped, check):
    projectable = [(prop, kind)] with kind `string` (JStr-narrowed to Maybe string), `bool`
    (JBool-narrowed to Maybe bool), or `json` (object/array/untyped — the raw sub-document as
    Maybe Json); skipped = [(prop, why)] — numeric properties are NOT projected: JNum carries an
    int or a float, so a typed numeric promise cannot be narrowed soundly by pattern alone.
    check = the conformance data the live gate holds the observed document to (required-present
    plus declared-type-per-present-property — exactly the shape the projections promise)."""
    props = schema.get("properties") or {}
    required = [r for r in (schema.get("required") or []) if isinstance(r, str)]
    projectable, skipped, types = [], [], {}
    for prop in props:  # declaration order
        psch = deref(spec, props[prop])
        if not isinstance(psch, dict):
            skipped.append((prop, "unresolvable property schema"))
            continue
        t = psch.get("type")
        if t:
            types[prop] = t
        if t == "string":
            projectable.append((prop, "string"))
        elif t == "boolean":
            projectable.append((prop, "bool"))
        elif t in ("integer", "number"):
            skipped.append((prop, f"declared `{t}` — JNum carries int or float, so a typed "
                                  "numeric promise cannot be narrowed soundly by pattern alone"))
        else:
            projectable.append((prop, "json"))
    return projectable, skipped, {"required": required, "types": types}


def _narrow_case(value_var, tag, bind_name):
    """`case v of { Tag(x) => Just x; _ => None }` — the sound narrowing from a Json variant to
    its payload type (JStr -> Maybe string, JBool -> Maybe bool)."""
    return {"kind": "case", "scrutinee": b_var(value_var),
            "arms": [
                {"pattern": {"kind": "variant", "tag": tag,
                             "payload": {"kind": "bind", "name": bind_name}},
                 "body": {"kind": "variant", "tag": "Just", "payload": b_var(bind_name)}},
                {"pattern": {"kind": "wildcard"}, "body": NONE_V},
            ]}


def _schema_projection_body(lam_params, call, status, field, kind):
    """A schema-derived projection body: the call bound ONCE, status-guarded to the response the
    schema is declared on. field=None -> the whole document (`parse_json r.body`, Maybe Json);
    otherwise map_get the field out of the JObj, narrowed per its declared type (`_narrow_case`),
    or raw for object/array/untyped fields — the json_get shape, inlined so the record is
    self-contained."""
    parsed = curried_app(b_var("parse_json"), b_field(b_var("r"), "body"))
    if field is None:
        on_status = parsed
    else:
        if kind == "string":
            get = {"kind": "case",
                   "scrutinee": curried_app(b_var("map_get"), s_lit(field), b_var("m")),
                   "arms": [
                       {"pattern": {"kind": "variant", "tag": "Just",
                                    "payload": {"kind": "bind", "name": "v"}},
                        "body": _narrow_case("v", "JStr", "s")},
                       {"pattern": {"kind": "wildcard"}, "body": NONE_V},
                   ]}
        elif kind == "bool":
            get = {"kind": "case",
                   "scrutinee": curried_app(b_var("map_get"), s_lit(field), b_var("m")),
                   "arms": [
                       {"pattern": {"kind": "variant", "tag": "Just",
                                    "payload": {"kind": "bind", "name": "v"}},
                        "body": _narrow_case("v", "JBool", "b")},
                       {"pattern": {"kind": "wildcard"}, "body": NONE_V},
                   ]}
        else:  # json: map_get already yields Maybe Json
            get = curried_app(b_var("map_get"), s_lit(field), b_var("m"))
        on_status = {"kind": "case", "scrutinee": parsed,
                     "arms": [
                         {"pattern": {"kind": "variant", "tag": "Just",
                                      "payload": {"kind": "bind", "name": "j"}},
                          "body": {"kind": "case", "scrutinee": b_var("j"),
                                   "arms": [
                                       {"pattern": {"kind": "variant", "tag": "JObj",
                                                    "payload": {"kind": "bind", "name": "m"}},
                                        "body": get},
                                       {"pattern": {"kind": "wildcard"}, "body": NONE_V},
                                   ]}},
                         {"pattern": {"kind": "wildcard"}, "body": NONE_V},
                     ]}
    return {"kind": "lambda",
            "params": [{"name": p} for p in lam_params],
            "body": b_let("r", call, _case_bool(
                curried_app(b_var("eq"), b_field(b_var("r"), "status"),
                            b_lit({"kind": "nat", "value": status})),
                on_status, NONE_V))}


def _value_conforms(doc, check):
    """Hold an OBSERVED document (evaluator value AST) to the declared schema fragment the
    projections were compiled from — and nothing more: object-ness, every required property
    present, every declared-type property that IS present carries its declared type. Deeper
    constraints (enum, minProperties, nested shapes) are deliberately unchecked: the gate
    checks exactly the shape the records promise. Returns (ok, why)."""
    if not (isinstance(doc, dict) and doc.get("tag") == "JObj"):
        return False, "observed document is not a JSON object"
    entries = {e["key"]: e["value"] for e in (doc.get("payload") or {}).get("entries", [])}
    for req in check["required"]:
        if req not in entries:
            return False, f"required property `{req}` absent from the observed document"
    tags = {"string": "JStr", "boolean": "JBool", "integer": "JNum", "number": "JNum",
            "object": "JObj", "array": "JList"}
    for prop, t in check["types"].items():
        if prop in entries and t in tags:
            v = entries[prop]
            if not (isinstance(v, dict) and v.get("tag") == tags[t]):
                return False, f"property `{prop}` is not the declared `{t}`"
            if t == "integer" and (v.get("payload") or {}).get("kind") != "int":
                return False, f"property `{prop}` is not an integer"
    return True, ""


def build_operation(spec, base_url, path, verb, op, shared_params, global_security, secret_name):
    """Compile one operation. Returns ("ok", records, notes, pending) or ("skip", op_id, reason).
    `pending` holds SCHEMA-DERIVED projections that cannot become records yet: a schema promises
    shape, not a value, so their worked example must be OBSERVED against a live service
    (--verify-against) before a v0.2 record (>=1 example) can exist at all."""
    verb = verb.upper()
    op_id = op.get("operationId") or _param_name(f"{verb}_{path}")

    # Operation-level parameters first, then path-item-level shared ones; the operation wins on a
    # (name, in) collision (the OpenAPI override rule). Every parameter may be a $ref.
    merged, seen = [], set()
    for p in list(op.get("parameters", [])) + list(shared_params):
        rp = deref(spec, p)
        if not isinstance(rp, dict):
            return ("skip", op_id, "unresolvable parameter $ref (external or dangling)")
        key = (rp.get("name"), rp.get("in"))
        if key in seen:
            continue
        seen.add(key)
        merged.append(rp)

    notes = []
    by_kind = {"path": [], "query": [], "header": []}
    for p in merged:
        where = p.get("in")
        if where == "cookie":
            return ("skip", op_id, "cookie parameter — out of the description subset")
        if where not in by_kind:
            continue
        if where != "path" and not p.get("required"):
            notes.append(f"optional {where} param `{p['name']}` omitted "
                         "(the record is the minimal documented call)")
            continue
        by_kind[where].append(p)
    path_param_names = [_param_name(p["name"]) for p in by_kind["path"]]
    query_ps, header_ps = by_kind["query"], by_kind["header"]

    body_spec = None
    mp_parts, mp_ct, mp_boundary = [], None, None
    if "requestBody" in op:
        body_spec = deref(spec, op["requestBody"])
        if not isinstance(body_spec, dict):
            return ("skip", op_id, "unresolvable requestBody $ref (external or dangling)")
        content = body_spec.get("content") or {}
        mp_types = sorted(ct for ct in content if ct.startswith("multipart/"))
        if content and len(mp_types) == len(content):
            # MULTIPART-ONLY body: compiled, not refused. The old refusal ("no deterministic
            # boundary construction") dissolves the same way url_encode's did — the boundary is a
            # SPEC-TIME constant (like a percent-encoded query-parameter name), the part names are
            # description data, and only the part VALUES are caller parameters. Required string
            # parts become parameters in declaration order; optional parts are omitted with a note
            # (the record is the minimal documented call). Honest limits keep refusing below.
            mp_ct = mp_types[0]
            schema = deref(spec, (content.get(mp_ct) or {}).get("schema") or {})
            props = (schema or {}).get("properties") or {}
            if not isinstance(schema, dict) or not props:
                return ("skip", op_id, "multipart body without declared part properties — "
                                       "no spec-time part names to build the form from")
            required = schema.get("required") or []
            if not required:
                return ("skip", op_id, "multipart body declares no required parts — an "
                                       "all-optional form has no minimal documented call")
            for pname in required:
                pschema = deref(spec, props.get(pname)) if pname in props else None
                if not isinstance(pschema, dict):
                    return ("skip", op_id, f"multipart required part `{pname}` undeclared or "
                                           "unresolvable")
                if pschema.get("type") != "string":
                    return ("skip", op_id, f"multipart part `{pname}` is not a string — only "
                                           "string parts (incl. format: binary) are representable")
            for pname in props:
                if pname not in required:
                    notes.append(f"optional multipart part `{pname}` omitted "
                                 "(the record is the minimal documented call)")
            mp_parts = [p for p in props if p in required]  # declaration order
            mp_boundary = f"nl-{_param_name(op_id)}-boundary"
            notes.append(f"multipart form: spec-time boundary `{mp_boundary}` rides in the "
                         "Content-Type literal; a part value containing the boundary delimiter "
                         "line would break framing (no escaping builtin — the caller's contract)")
            body_spec = None
    has_body = body_spec is not None

    auth = resolve_auth(spec, op, global_security, secret_name)
    if auth[0] == "refuse":
        return ("skip", op_id, auth[1])

    query_names = [_param_name(p["name"]) for p in query_ps]
    header_names = [_param_name(p["name"]) for p in header_ps]
    lam_params = (["base"] + path_param_names + query_names + header_names
                  + (["body"] if has_body else [_param_name(p) for p in mp_parts]))
    for name in lam_params:
        if lam_params.count(name) > 1:
            return ("skip", op_id, f"parameter name collision on `{name}`")

    url = url_builder(b_var("base"), path)
    if query_ps:
        # `?k1=` v1 `&k2=` v2 … — names are spec-time literals (percent-encoded here, at
        # generation time); values are caller data, encoded at RUN time by the url_encode
        # builtin (or rendered by to_string for an integer schema — digits are unreserved).
        qtokens = []
        for i, p in enumerate(query_ps):
            qtokens.append(s_lit(("?" if i == 0 else "&") + _pct(p["name"]) + "="))
            v = b_var(_param_name(p["name"]))
            encoder = "to_string" if _schema_type(spec, p) == "integer" else "url_encode"
            qtokens.append(curried_app(b_var(encoder), v))
        url = curried_app(b_var("str_concat"), url, str_concat_chain(qtokens))

    headers = b_var("map_empty")
    if auth[0] == "header":
        hname, hvalue = auth[1]
        headers = curried_app(b_var("map_put"), s_lit(hname), s_lit(hvalue), headers)
    for p in reversed(header_ps):  # first-declared outermost — a stable, readable order
        headers = curried_app(b_var("map_put"), s_lit(p["name"]),
                              b_var(_param_name(p["name"])), headers)
    if mp_ct:
        headers = curried_app(b_var("map_put"), s_lit("Content-Type"),
                              s_lit(f"{mp_ct}; boundary={mp_boundary}"), headers)

    if has_body:
        body_arg = b_var("body")
    elif mp_parts:
        # The RFC 2046 form body as a str_concat chain: framing (boundary lines, per-part
        # Content-Disposition with the SPEC-TIME part name) is literal; only part VALUES are
        # caller parameters — the same literal-scaffold/caller-data split as the URL builder.
        tokens = []
        for pname in mp_parts:
            tokens.append(s_lit(f"--{mp_boundary}\r\n"
                                f'Content-Disposition: form-data; name="{pname}"\r\n\r\n'))
            tokens.append(b_var(_param_name(pname)))
            tokens.append(s_lit("\r\n"))
        tokens.append(s_lit(f"--{mp_boundary}--\r\n"))
        body_arg = str_concat_chain(tokens)
    else:
        body_arg = s_lit("")
    call = curried_app(b_var("http"), s_lit(verb), url, headers, body_arg)
    body_ast = {"kind": "lambda",
                "params": [{"name": p} for p in lam_params],
                "body": b_field(call, "status")}

    int_params = {_param_name(p["name"]) for p in query_ps if _schema_type(spec, p) == "integer"}
    param_types = [INT if p in int_params else STRING for p in lam_params]
    type_ast = {"kind": "fn", "params": param_types, "result": INT}
    effect = "net.read" if verb in _READ_VERBS else "net.write"

    example = _example_for(base_url, verb, path_param_names, has_body, op,
                           query_ps=query_ps, header_ps=header_ps, int_params=int_params, spec=spec,
                           mp_parts=mp_parts)
    intent = ["io", "io/network/http"] + (["query/lookup"] if verb in _READ_VERBS else [])
    # Discovery precision (the GitHub-scale finding: 10 same-sort effectful fits the rank could
    # not split, because every generated record carried the SAME four tags): each record gets ONE
    # extending tag — `<lead>/<its-own-hyphenated-name>` — so a caller can query precisely (the
    # GW3 blessed-tag move, applied at generation time), the rank's tag-specificity rewards it
    # under broad queries, and the specific intent's tokens feed the name-affinity signal.
    lead = "query/lookup" if verb in _READ_VERBS else "io/network/http"

    record = build_v2_record(
        name=op_id, type_ast=type_ast, examples=[example], body_text=body_ast,
        module_name=None, extra_hints=[_param_name(op_id)],
        effects=[effect], terminates="always",
        intent_tags=intent + _intent_ext(lead, op_id), complexity="O(n)",
    )
    records = [(record, body_ast)]

    # BODY PROJECTION (the GW7 residual, unblocked by observed claims): a documented 2xx
    # application/json example is spec-time knowledge of the payload, so it can gate a second
    # record `<opId>Body : … -> Maybe Json` — parse_json over the response body. Only where a
    # deterministic SUCCESS example is constructible from the spec alone: a GET with no path
    # parameters and no request body (path parameters name server state the description cannot
    # promise). Field access composes in-language (json_get / json_path), principle 4.
    pending = []
    proj = _response_json_example(spec, op)
    if proj is not None:
        proj_code, proj_doc = proj
        expected = _json_to_value(proj_doc)
        if verb != "GET" or path_param_names or has_body:
            notes.append("documented response example not projected — only a bodyless GET "
                         "without path parameters has a spec-constructible success example")
        elif expected is None:
            notes.append("documented response example not projected — it contains a "
                         "non-integer number (a float-equality promise we refuse to invent)")
        else:
            proj_body = {"kind": "lambda",
                         "params": [{"name": p} for p in lam_params],
                         "body": curried_app(b_var("parse_json"), b_field(call, "body"))}
            proj_example = {"args": example["args"],
                            "result": {"kind": "variant", "tag": "Just", "payload": expected}}
            proj_record = build_v2_record(
                name=op_id + "Body", type_ast={"kind": "fn", "params": param_types,
                                               "result": MAYBE_JSON},
                examples=[proj_example], body_text=proj_body,
                module_name=None, extra_hints=[_param_name(op_id + "Body")],
                effects=[effect], terminates="always",
                intent_tags=intent + ["parse"] + _intent_ext("parse", op_id + "Body"),
                complexity="O(n)",
            )
            records.append((proj_record, proj_body))
            notes.append(f"body projection: {op_id}Body -> Maybe Json "
                         f"(documented {proj_code} example)")
    else:
        # SCHEMA-DERIVED PROJECTION (the ingestion-sweep residual — the Frankfurter finding:
        # real-world descriptions declare response SCHEMAS, not examples). A declared 2xx
        # object schema licenses `<opId>Body : … -> Maybe Json` plus one typed field
        # projection per soundly-narrowable declared property — but a schema promises SHAPE,
        # not a value, so these stay PENDING: the live gate observes the value (one execution,
        # trace-carried, offline-replayable) and holds the observed document to the declared
        # shape — a lying description refuses rather than publishes. Same constructibility
        # constraint as the example path, same one-bound-call shape as the header projections.
        sch = _response_json_schema(spec, op)
        if sch is not None:
            s_code, s_schema = sch
            if verb != "GET" or path_param_names or has_body:
                notes.append("declared response schema not projected — only a bodyless GET "
                             "without path parameters has a spec-constructible success call")
            elif s_schema.get("type") != "object" and not s_schema.get("properties"):
                notes.append("declared response schema not projected — this increment "
                             "projects object documents only")
            else:
                fields, skipped_fields, check = _schema_object_fields(spec, s_schema)
                for prop, why in skipped_fields:
                    notes.append(f"schema property `{prop}` not projected — {why}")
                if not fields:
                    notes.append("schema declares no named properties (a map-shaped object) "
                                 "— whole-document projection only")
                pending.append({
                    "name": op_id + "Body", "hint": _param_name(op_id + "Body"),
                    "type_ast": {"kind": "fn", "params": param_types, "result": MAYBE_JSON},
                    "body_ast": _schema_projection_body(lam_params, call, s_code, None, None),
                    "args": example["args"], "effect": effect,
                    "intent": intent + ["parse"] + _intent_ext("parse", op_id + "Body"),
                    "field": None, "required_field": False,
                    "check": check, "code": s_code,
                })
                kinds = {"string": MAYBE_STRING, "bool": MAYBE_BOOL, "json": MAYBE_JSON}
                for prop, kind in fields:
                    suffix = re.sub(r"[^a-zA-Z0-9]", " ", prop).title().replace(" ", "")
                    pending.append({
                        "name": op_id + suffix, "hint": _param_name(op_id + suffix),
                        "type_ast": {"kind": "fn", "params": param_types,
                                     "result": kinds[kind]},
                        "body_ast": _schema_projection_body(lam_params, call, s_code,
                                                            prop, kind),
                        "args": example["args"], "effect": effect,
                        "intent": intent + ["parse"] + _intent_ext("parse", op_id + suffix),
                        "field": prop,
                        "required_field": prop in check["required"],
                        "check": check, "code": s_code,
                    })
                declared = ", ".join(f"{p}:{k}" for p, k in fields) or "(none)"
                notes.append(f"schema-derived projections pending a live observation gate: "
                             f"{op_id}Body -> Maybe Json; fields {declared} "
                             f"(declared {s_code} schema)")

    # HEADER PROJECTION (GW16 — the description-layer counterpart of the GW14 pull): a header the
    # documented response declares WITH an example is spec-time knowledge of where the answer
    # arrives (Location being the canonical case — server-assigned identity, redirect targets),
    # so it gates a record `<opId><Header> : … -> Maybe string` over `http_full`: the call bound
    # ONCE (projecting two fields off two calls would perform the effect twice), status-guarded
    # to exactly the documented response, `map_get` of the LOWERCASE name (the canonical decode).
    # A declared header without an example is noted, never guessed; with `--verify-against`, the
    # live gate proves the documented example is what the service really answers.
    want = example["result"]["value"]
    projectable, unprojectable = _response_header_examples(spec, op, want)
    for hname in unprojectable:
        notes.append(f"response header `{hname}` not projected — no documented example "
                     "(a promise the description does not make)")
    for hname, hexample in projectable:
        suffix = re.sub(r"[^a-zA-Z0-9]", " ", hname).title().replace(" ", "")
        call_full = curried_app(b_var("http_full"), s_lit(verb), url, headers, body_arg)
        hdr_body = {
            "kind": "lambda",
            "params": [{"name": p} for p in lam_params],
            "body": b_let("r", call_full, _case_bool(
                curried_app(b_var("eq"), b_field(b_var("r"), "status"),
                            # `nat`, not `int`: a non-negative literal parses to nat, so the
                            # generated guard matches a hand-written `case eq r.status 201` exactly.
                            b_lit({"kind": "nat", "value": want})),
                curried_app(b_var("map_get"), s_lit(hname.lower()), b_field(b_var("r"), "headers")),
                {"kind": "variant", "tag": "None"})),
        }
        hdr_example = {"args": example["args"],
                       "result": {"kind": "variant", "tag": "Just",
                                  "payload": {"kind": "string", "value": hexample}}}
        hdr_record = build_v2_record(
            name=op_id + suffix, type_ast={"kind": "fn", "params": param_types,
                                           "result": MAYBE_STRING},
            examples=[hdr_example], body_text=hdr_body,
            module_name=None, extra_hints=[_param_name(op_id + suffix)],
            effects=[effect], terminates="always",
            intent_tags=intent + _intent_ext(lead, op_id + suffix), complexity="O(n)",
        )
        records.append((hdr_record, hdr_body))
        notes.append(f"header projection: {op_id}{suffix} -> Maybe string "
                     f"(documented {want} `{hname}` example)")
    return ("ok", records, notes, pending)


def _example_for(base_url, verb, path_param_names, has_body, op,
                 query_ps=(), header_ps=(), int_params=frozenset(), spec=None, mp_parts=()):
    """One worked example whose expected status is a DOCUMENTED, deterministic outcome: a read/delete
    on a guaranteed-absent name is the documented 404; a create is the documented 2xx; a bodyless
    probe is its single documented status. Verified live with `--verify-against`. A string query
    value deliberately contains a SPACE — the live check then passes only if url_encode really
    ran (a raw space is a malformed request target)."""
    codes = sorted(int(c) for c in op.get("responses", {}) if c.isdigit())
    args = [{"kind": "string", "value": base_url}]
    # A name no test writes, so GET/DELETE are the documented absent-case; PUT/POST create it fresh.
    absent = "gw7-absent-x"
    for _ in path_param_names:
        args.append({"kind": "string", "value": absent if verb in ("GET", "DELETE") else "gw7-new"})
    for p in query_ps:
        if _param_name(p["name"]) in int_params:
            args.append({"kind": "int", "value": 5})
        else:
            args.append({"kind": "string", "value": "hello world"})
    for _ in header_ps:
        args.append({"kind": "string", "value": "gw10-client"})
    if has_body:
        args.append({"kind": "string", "value": "{}"})
    for pname in mp_parts:
        args.append({"kind": "string", "value": f"{pname} bytes"})
    if verb in ("GET", "DELETE"):
        if path_param_names:
            want = 404 if 404 in codes else (codes[0] if codes else 200)
        else:
            # No path parameter carries the guaranteed-absent name, so the minimal call IS the
            # happy path — assert the documented success, not a 404 the call cannot reach.
            # (Found by the Frankfurter live gate: GET /latest documents a 404 for date paths,
            # but the parameterless call answers 200.)
            want = next((c for c in codes if 200 <= c < 300), codes[0] if codes else 200)
    elif verb in ("PUT", "POST"):
        # A fresh name is a CREATE — 201 if the operation documents it, else the first 2xx.
        want = 201 if 201 in codes else next((c for c in codes if 200 <= c < 300), codes[0] if codes else 201)
    else:
        want = codes[0] if codes else 200
    return {"args": args, "result": {"kind": "int", "value": want}}


def walk(spec, secret_name):
    """-> (built, skipped, pending): built = [(record, body_ast, notes)], skipped =
    [(op_id, reason)], pending = schema-derived projections awaiting a live observation gate."""
    base_url = (spec.get("servers") or [{}])[0].get("url", "http://localhost")
    global_security = spec.get("security", [])
    built, skipped, pending = [], [], []
    for path, item in spec.get("paths", {}).items():
        item = deref(spec, item)
        if not isinstance(item, dict):
            skipped.append((path, "unresolvable path-item $ref"))
            continue
        shared_params = item.get("parameters", [])
        for verb, op in item.items():
            if verb.lower() not in ("get", "put", "post", "delete", "head", "patch"):
                continue
            got = build_operation(spec, base_url, path, verb, op, shared_params,
                                  global_security, secret_name)
            if got[0] == "ok":
                # One operation may compile to several records (status + body projection);
                # the notes ride with the first so they print once.
                for i, (record, body_ast) in enumerate(got[1]):
                    built.append((record, body_ast, got[2] if i == 0 else []))
                pending.extend(got[3])
            else:
                skipped.append((got[1], got[2]))
    return built, skipped, pending


# Above this many JCS-canonical bytes an example's expected value is carried BY ADDRESS
# (`result_blob`, function-record v0.2): the value's canonical bytes become a `blob-<sha256>.json`
# sidecar (destined for the gate-free /v0/blobs store — the weights boundary: host untrusted, the
# sha256 in the record is the truth) and the record carries only the pointer. Chosen well under the
# reference node's 1 MiB record-store cap: a multi-MB observed document (the NWS glossary) must not
# blow the record gate, while ordinary worked examples stay inline and human-greppable.
BLOB_THRESHOLD_DEFAULT = 65536


def blobify_example(ex, out_dir, threshold):
    """Move an oversized inline `result` out to a verified blob sidecar. Returns the sha256 when
    the example was converted, else None (small values stay inline)."""
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
    r = subprocess.run(
        [_VALIDATOR, "certify", record_path, "--body", body_path, "--records", out_dir],
        capture_output=True, text=True,
    )
    return r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip()


def attach_example_traces(record_path, body_path, record, out_dir, secret_names, token,
                          oauth_ids=(), blob_threshold=BLOB_THRESHOLD_DEFAULT):
    """The live gate for an EFFECTFUL record, one execution per example (GW12): run each worked
    example once via `eval --trace-out` (grants = the record's declared effects, secrets supplied),
    require the live result to equal the documented one, and attach the observed trace to the
    example by `trc_…` content-address — writing the trace artifact alongside the record. The
    record's examples change, so its content-address is recomputed. After this, `run` checks the
    examples by REPLAY: no grants, no secrets, no live service — a commons consumer can verify the
    record offline. Returns (ok, message)."""
    name = record["name_hints"][0]
    effects = (record.get("signature") or {}).get("effects") or []
    for i, ex in enumerate(record.get("examples", [])):
        argfiles = []
        for j, a in enumerate(ex.get("args", [])):
            p = os.path.join(out_dir, f".arg-{name}-{i}-{j}.json")
            json.dump(a, open(p, "w"))
            argfiles.append(p)
        trace_path = os.path.join(out_dir, f"trace-{name}-{i}.json")
        cmd = [_VALIDATOR, "eval", body_path]
        for p in argfiles:
            cmd += ["--arg", p]
        for e in effects:
            cmd += ["--grant", e]
        for n in secret_names:
            cmd += ["--secret", f"{n}={token}"]
        for oname, cfg in oauth_ids:
            cmd += ["--oauth", f"{oname}={cfg}"]
        cmd += ["--trace-out", trace_path]
        r = subprocess.run(cmd, capture_output=True, text=True)
        for p in argfiles:
            os.unlink(p)
        if r.returncode != 0:
            return False, f"example {i} live run failed: {(r.stderr or '').strip()}"
        got = json.loads(r.stdout)
        if got != ex.get("result"):
            return False, (f"example {i} live result does not match the documented one: "
                           f"{json.dumps(got)} != {json.dumps(ex.get('result'))}")
        trc = subprocess.run([_VALIDATOR, "hash", trace_path],
                             capture_output=True, text=True).stdout.strip()
        if not trc.startswith("trc_"):
            return False, f"example {i} trace did not hash to a trc_… address: {trc!r}"
        ex["trace"] = trc
        blobify_example(ex, out_dir, blob_threshold)
    # The examples changed -> the record's content-address moves; recompute and rewrite.
    record.pop("hash", None)
    json.dump(record, open(record_path, "w"), indent=2)
    new_hash = subprocess.run([_VALIDATOR, "hash", record_path],
                              capture_output=True, text=True).stdout.strip()
    record["hash"] = new_hash
    json.dump(record, open(record_path, "w"), indent=2)
    return True, new_hash


def materialize_schema_projection(p, out_dir, secret_names, token, oauth_ids=(),
                                  blob_threshold=BLOB_THRESHOLD_DEFAULT):
    """The live OBSERVATION gate for a schema-derived projection (the GW12 shape, one rung
    deeper): run the body once (`eval --trace-out`, grants + secrets), hold the observed value
    to the DECLARED schema — the whole document to `_value_conforms`, a required field to
    presence-with-its-declared-type (`Just …`, never `None`) — and only then does a record
    exist at all: the observation becomes its worked example, trace-attached, so `run` replays
    it offline. A service that does not honor its description FAILS here; nothing publishes.
    Returns (ok, record_or_message)."""
    name = p["name"]
    fbase = sanitize_hint(name)  # the on-disk convention: lowercase name_hints[0]
    bp = os.path.join(out_dir, f"body-{fbase}.json")
    json.dump(p["body_ast"], open(bp, "w"), indent=2)
    argfiles = []
    for j, a in enumerate(p["args"]):
        ap = os.path.join(out_dir, f".arg-{fbase}-{j}.json")
        json.dump(a, open(ap, "w"))
        argfiles.append(ap)
    trace_path = os.path.join(out_dir, f"trace-{fbase}-0.json")
    cmd = [_VALIDATOR, "eval", bp]
    for ap in argfiles:
        cmd += ["--arg", ap]
    cmd += ["--grant", p["effect"]]
    for n in secret_names:
        cmd += ["--secret", f"{n}={token}"]
    for oname, cfg in oauth_ids:
        cmd += ["--oauth", f"{oname}={cfg}"]
    cmd += ["--trace-out", trace_path]
    r = subprocess.run(cmd, capture_output=True, text=True)
    for ap in argfiles:
        os.unlink(ap)
    if r.returncode != 0:
        return False, f"live observation failed: {(r.stderr or '').strip()}"
    got = json.loads(r.stdout)
    is_none = isinstance(got, dict) and got.get("kind") == "variant" and got.get("tag") == "None"
    if p["field"] is None:
        if is_none:
            return False, (f"live response did not yield the declared {p['code']} JSON "
                           "document (status or parse mismatch)")
        ok, why = _value_conforms(got.get("payload"), p["check"])
        if not ok:
            return False, f"observed document violates the declared schema: {why}"
    elif p["required_field"] and is_none:
        return False, (f"required property `{p['field']}` absent or mistyped in the live "
                       "response — the description's promise does not hold")
    trc = subprocess.run([_VALIDATOR, "hash", trace_path],
                         capture_output=True, text=True).stdout.strip()
    if not trc.startswith("trc_"):
        return False, f"trace did not hash to a trc_… address: {trc!r}"
    example = {"args": p["args"], "result": got, "trace": trc}
    blobify_example(example, out_dir, blob_threshold)
    record = build_v2_record(
        name=name, type_ast=p["type_ast"],
        examples=[example],
        body_text=p["body_ast"], module_name=None, extra_hints=[p["hint"]],
        effects=[p["effect"]], terminates="always", intent_tags=p["intent"],
        complexity="O(n)",
    )
    rp = os.path.join(out_dir, f"{fbase}.v0.2.json")
    json.dump(record, open(rp, "w"), indent=2)
    return True, record


def verify_examples(record_path, out_dir, base_url, secret_names, token):
    secret_flags = [f for n in secret_names for f in ("--secret", f"{n}={token}")]
    r = subprocess.run(
        [_VALIDATOR, "run", record_path, "--records", out_dir] + secret_flags,
        capture_output=True, text=True,
        env={**os.environ},
    )
    # `run` grants the record's declared effects; but net grants there are host-agnostic (the record
    # declares `net.read`/`net.write`, not a host), so no extra flag is needed for a live check.
    return r.returncode == 0, (r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip())


def main(argv=None):
    ap = argparse.ArgumentParser(description="Generate verified Nova Lingua records from OpenAPI 3.")
    ap.add_argument("spec", help="path to an OpenAPI 3 JSON description")
    ap.add_argument("--out", required=True, help="output directory for records + bodies")
    ap.add_argument("--secret-name", default=None,
                    help="secret placeholder name for Bearer auth (default: the security scheme name)")
    ap.add_argument("--verify-against", default=None,
                    help="run each record's examples against this live base URL (a fake or real service)")
    ap.add_argument("--token", default="test-token", help="Bearer token for --verify-against")
    ap.add_argument("--oauth-client", default="gw13-client:gw13-secret",
                    help="client-credentials pair (id:secret) for oauth2 schemes during --verify-against; "
                         "the tokenUrl comes from the description itself")
    ap.add_argument("--blob-threshold", type=int, default=BLOB_THRESHOLD_DEFAULT,
                    help="JCS-canonical bytes above which an example's expected value is carried by "
                         "address (a result_blob pointer + a blob-<sha256>.json sidecar for the "
                         f"gate-free /v0/blobs store; default {BLOB_THRESHOLD_DEFAULT})")
    args = ap.parse_args(argv)

    spec = load_spec(args.spec)
    # The record-side default is the security-scheme KEY (per scheme, in resolve_auth);
    # --secret-name overrides. The live check supplies `--secret NAME=token` for each.
    schemes = (spec.get("components") or {}).get("securitySchemes") or {}
    secret_name = args.secret_name
    secret_names = [secret_name] if secret_name else (list(schemes) or ["api_token"])
    # OAuth2 client-credentials identities for the live gate: identity name = the scheme key
    # (matching the {{oauth:<key>}} the records carry), tokenUrl from the DESCRIPTION itself,
    # client id/secret from the operator (--oauth-client) — the same division as --secret.
    cid, _, csec = args.oauth_client.partition(":")
    oauth_ids = []
    for key, sch in schemes.items():
        sch = deref(spec, sch)
        if isinstance(sch, dict) and sch.get("type") == "oauth2":
            cc = (sch.get("flows") or {}).get("clientCredentials") or {}
            if cc.get("tokenUrl"):
                oauth_ids.append((secret_name or key, f"{cc['tokenUrl']}|{cid}|{csec}"))
    os.makedirs(args.out, exist_ok=True)

    ops, skipped, pending = walk(spec, secret_name)
    for op_id, reason in skipped:
        print(f"{op_id:16} SKIPPED: {reason}")
    if not ops:
        sys.exit("no supported operations found in the spec")

    # Write records + bodies first (so certify can resolve body_hash via --records), then gate.
    written = []
    for record, body_ast, notes in ops:
        name = record["name_hints"][0]
        for note in notes:
            print(f"{name:16} note: {note}")
        rp = os.path.join(args.out, f"{name}.v0.2.json")
        bp = os.path.join(args.out, f"body-{name}.json")
        json.dump(record, open(rp, "w"), indent=2)
        json.dump(body_ast, open(bp, "w"), indent=2)
        written.append((name, rp, bp, record))

    ok = True
    # Schema-derived projections: a schema promises shape, not a value — without a live gate
    # there is nothing honest to put in the record's example, so they are not emitted at all.
    observed = set()
    if pending and not args.verify_against:
        for p in pending:
            print(f"{p['name']:16} note: schema-derived projection not materialized — a schema "
                  "promises shape, not a value; --verify-against observes one")
    elif pending:
        for p in pending:
            m_ok, got = materialize_schema_projection(p, args.out, secret_names, args.token,
                                                      oauth_ids=oauth_ids,
                                                      blob_threshold=args.blob_threshold)
            if not m_ok:
                ok = False
                print(f"{p['name']:16} observation-gate=FAIL: {got}")
                continue
            name = got["name_hints"][0]
            rp = os.path.join(args.out, f"{name}.v0.2.json")
            bp = os.path.join(args.out, f"body-{name}.json")
            written.append((name, rp, bp, got))
            observed.add(name)

    for name, rp, bp, record in written:
        line = f"{name:16} body={record['body_hash'][:20]}…"
        effectful = bool((record.get("signature") or {}).get("effects"))
        if name in observed:
            # Already ran live exactly once (the observation IS the example, trace attached);
            # certify + the offline replay below are the remaining checks.
            line += "  live=OBSERVED+schema-checked"
        elif args.verify_against and effectful:
            # GW12: the live gate for an effectful record IS the trace capture — each example runs
            # exactly once (grants + secrets), must reproduce its documented result, and its
            # observed trace is attached by trc_… address (re-addressing the record).
            att_ok, att_msg = attach_example_traces(rp, bp, record, args.out, secret_names, args.token,
                                                    oauth_ids=oauth_ids,
                                                    blob_threshold=args.blob_threshold)
            if not att_ok:
                ok = False
                print(f"{line}  live-gate=FAIL: {att_msg}")
                continue
            line += "  live=PASS+traces"
        # After the gates (either one may have moved an oversized expected value out to a blob
        # sidecar), report examples that are now carried by address.
        blobbed = [ex["result_blob"] for ex in record.get("examples", []) if "result_blob" in ex]
        if blobbed:
            line += f"  example=BY-ADDRESS({'+'.join(str(b['bytes']) for b in blobbed)} bytes)"
        cert_ok, cert_msg = certify(rp, bp, args.out)
        line += f"  certify={'OK' if cert_ok else 'FAIL'}"
        ok = ok and cert_ok
        if args.verify_against:
            # For an effectful record this now REPLAYS from the attached traces — deliberately
            # run with NO secrets, proving the offline check a commons consumer performs needs
            # neither credentials nor the service. A pure record just runs as before.
            run_ok, run_msg = verify_examples(rp, args.out, args.verify_against, [], "")
            label = "PASS (replayed offline)" if effectful else "PASS"
            line += f"  examples={label if run_ok else 'FAIL: ' + run_msg}"
            ok = ok and run_ok
        print(line)
        if not cert_ok:
            print(f"    {cert_msg}")

    placeholders = ", ".join(f"{{{{secret:{n}}}}}" for n in secret_names)
    print(f"\nwrote {len(written)} records -> {args.out}  (secret placeholders: {placeholders})")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
