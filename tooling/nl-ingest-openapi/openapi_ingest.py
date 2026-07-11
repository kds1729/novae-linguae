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
    requestBody  -> a `body` string parameter (omitted for bodyless verbs); a multipart-only
                    body REFUSES the operation (no deterministic boundary construction)
    security     -> `http`/`bearer` -> an `Authorization: Bearer {{secret:NAME}}` header;
                    `apiKey` in `header` -> a `<name>: {{secret:NAME}}` header. apiKey in
                    query/cookie is REFUSED: a secret placeholder substitutes only inside a
                    HEADER value at the effect boundary (GW6) — in a query string it would enter
                    the URL, hence the record and the trace. HTTP basic (needs base64 — no
                    builtin) and oauth2/openIdConnect flows are refused too
    $ref         -> local `#/...` references (parameters, requestBodies, responses, security
                    schemes, path-item-level shared parameters) are resolved; an external or
                    dangling reference refuses the operation honestly
    responses    -> the documented status the worked example asserts; a 2xx response that
                    documents an application/json EXAMPLE additionally yields a body-projection
                    record `<opId>Body : … -> Maybe Json` (`parse_json` over the response body —
                    field access then composes in-language via the certified json_get/json_path
                    commons records, principle 4). Emitted only when a deterministic success
                    example is constructible from the spec alone (a GET with no path parameters);
                    anything else gets a printed note, never a silent guess.

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
import json
import os
import re
import subprocess
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.normpath(os.path.join(_HERE, "..", "ingest-common")))

from nl_body import b_app, b_field, b_lit, b_var  # noqa: E402
from nl_core import build_v2_record, expr_address  # noqa: E402

STRING = {"kind": "builtin", "name": "string"}
INT = {"kind": "builtin", "name": "int"}
JSON_T = {"kind": "builtin", "name": "Json"}
# The structural sum {Just Json, None} — how a Maybe result is declared (the json_get precedent).
MAYBE_JSON = {"kind": "sum", "variants": [{"tag": "Just", "type": JSON_T}, {"tag": "None"}]}
_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator")
)
_READ_VERBS = {"GET", "HEAD"}


def s_lit(text):
    return b_lit({"kind": "string", "value": text})


def deref(spec, node, _depth=0):
    """Resolve a local `#/...` $ref chain (JSON Pointer, RFC 6901 `~0`/`~1` escaping) against the
    spec document. Returns None for an external, dangling, or cyclic reference — the caller
    refuses the operation honestly rather than guessing."""
    while isinstance(node, dict) and "$ref" in node:
        if _depth > 32:
            return None
        ref = node["$ref"]
        if not isinstance(ref, str) or not ref.startswith("#/"):
            return None
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


def _response_json_example(spec, op):
    """The documented application/json EXAMPLE of the operation's first 2xx response, as
    (status_code, parsed_example), or None. This is spec-time data — the one thing that makes a
    runnable worked example for a body projection possible without guessing."""
    responses = op.get("responses", {})
    for code in sorted(c for c in responses if c.isdigit() and 200 <= int(c) < 300):
        resp = deref(spec, responses[code])
        if not isinstance(resp, dict):
            continue
        media = (resp.get("content") or {}).get("application/json") or {}
        example = media.get("example")
        if example is None and isinstance(media.get("schema"), dict):
            example = media["schema"].get("example")
        if example is not None:
            return int(code), example
    return None


def build_operation(spec, base_url, path, verb, op, shared_params, global_security, secret_name):
    """Compile one operation. Returns ("ok", record, body_ast, notes) or ("skip", op_id, reason)."""
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
    if "requestBody" in op:
        body_spec = deref(spec, op["requestBody"])
        if not isinstance(body_spec, dict):
            return ("skip", op_id, "unresolvable requestBody $ref (external or dangling)")
        content = body_spec.get("content") or {}
        if content and all(ct.startswith("multipart/") for ct in content):
            return ("skip", op_id, "multipart-only request body — no deterministic "
                                   "boundary construction")
    has_body = body_spec is not None

    auth = resolve_auth(spec, op, global_security, secret_name)
    if auth[0] == "refuse":
        return ("skip", op_id, auth[1])

    query_names = [_param_name(p["name"]) for p in query_ps]
    header_names = [_param_name(p["name"]) for p in header_ps]
    lam_params = (["base"] + path_param_names + query_names + header_names
                  + (["body"] if has_body else []))
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

    body_arg = b_var("body") if has_body else s_lit("")
    call = curried_app(b_var("http"), s_lit(verb), url, headers, body_arg)
    body_ast = {"kind": "lambda",
                "params": [{"name": p} for p in lam_params],
                "body": b_field(call, "status")}

    int_params = {_param_name(p["name"]) for p in query_ps if _schema_type(spec, p) == "integer"}
    param_types = [INT if p in int_params else STRING for p in lam_params]
    type_ast = {"kind": "fn", "params": param_types, "result": INT}
    effect = "net.read" if verb in _READ_VERBS else "net.write"

    example = _example_for(base_url, verb, path_param_names, has_body, op,
                           query_ps=query_ps, header_ps=header_ps, int_params=int_params, spec=spec)
    intent = ["io", "io/network/http"] + (["query/lookup"] if verb in _READ_VERBS else [])

    record = build_v2_record(
        name=op_id, type_ast=type_ast, examples=[example], body_text=body_ast,
        module_name=None, extra_hints=[_param_name(op_id)],
        effects=[effect], terminates="always", intent_tags=intent, complexity="O(n)",
    )
    records = [(record, body_ast)]

    # BODY PROJECTION (the GW7 residual, unblocked by observed claims): a documented 2xx
    # application/json example is spec-time knowledge of the payload, so it can gate a second
    # record `<opId>Body : … -> Maybe Json` — parse_json over the response body. Only where a
    # deterministic SUCCESS example is constructible from the spec alone: a GET with no path
    # parameters and no request body (path parameters name server state the description cannot
    # promise). Field access composes in-language (json_get / json_path), principle 4.
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
                effects=[effect], terminates="always", intent_tags=intent + ["parse"],
                complexity="O(n)",
            )
            records.append((proj_record, proj_body))
            notes.append(f"body projection: {op_id}Body -> Maybe Json "
                         f"(documented {proj_code} example)")
    return ("ok", records, notes)


def _example_for(base_url, verb, path_param_names, has_body, op,
                 query_ps=(), header_ps=(), int_params=frozenset(), spec=None):
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
    if verb in ("GET", "DELETE"):
        want = 404 if 404 in codes else (codes[0] if codes else 200)
    elif verb in ("PUT", "POST"):
        # A fresh name is a CREATE — 201 if the operation documents it, else the first 2xx.
        want = 201 if 201 in codes else next((c for c in codes if 200 <= c < 300), codes[0] if codes else 201)
    else:
        want = codes[0] if codes else 200
    return {"args": args, "result": {"kind": "int", "value": want}}


def walk(spec, secret_name):
    """-> (built, skipped): built = [(record, body_ast, notes)], skipped = [(op_id, reason)]."""
    base_url = (spec.get("servers") or [{}])[0].get("url", "http://localhost")
    global_security = spec.get("security", [])
    built, skipped = [], []
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
            else:
                skipped.append((got[1], got[2]))
    return built, skipped


def certify(record_path, body_path, out_dir):
    r = subprocess.run(
        [_VALIDATOR, "certify", record_path, "--body", body_path, "--records", out_dir],
        capture_output=True, text=True,
    )
    return r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip()


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
    args = ap.parse_args(argv)

    spec = json.load(open(args.spec))
    # The record-side default is the security-scheme KEY (per scheme, in resolve_auth);
    # --secret-name overrides. The live check supplies `--secret NAME=token` for each.
    schemes = (spec.get("components") or {}).get("securitySchemes") or {}
    secret_name = args.secret_name
    secret_names = [secret_name] if secret_name else (list(schemes) or ["api_token"])
    os.makedirs(args.out, exist_ok=True)

    ops, skipped = walk(spec, secret_name)
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
    for name, rp, bp, record in written:
        cert_ok, cert_msg = certify(rp, bp, args.out)
        line = f"{name:16} body={record['body_hash'][:20]}…  certify={'OK' if cert_ok else 'FAIL'}"
        if args.verify_against:
            run_ok, run_msg = verify_examples(rp, args.out, args.verify_against, secret_names, args.token)
            line += f"  examples={'PASS' if run_ok else 'FAIL: ' + run_msg}"
            ok = ok and run_ok
        ok = ok and cert_ok
        print(line)
        if not cert_ok:
            print(f"    {cert_msg}")

    placeholders = ", ".join(f"{{{{secret:{n}}}}}" for n in secret_names)
    print(f"\nwrote {len(written)} records -> {args.out}  (secret placeholders: {placeholders})")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
