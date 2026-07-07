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
    requestBody  -> a `body` string parameter (omitted for bodyless verbs)
    security     -> an `Authorization: Bearer {{secret:NAME}}` header (empty security => no header)
    responses    -> the documented status the worked example asserts

Each generated record RETURNS the response `.status` (an int) — the deterministic, verifiable part
of a response; projecting the body (server-assigned, nondeterministic) waits for observed-claims.
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
_VALIDATOR = os.path.normpath(
    os.path.join(_HERE, "..", "validator", "target", "release", "nl-validator")
)
_READ_VERBS = {"GET", "HEAD"}


def s_lit(text):
    return b_lit({"kind": "string", "value": text})


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


def auth_header_map(secret_name):
    """`map_put "Authorization" "Bearer {{secret:NAME}}" map_empty`."""
    return curried_app(
        b_var("map_put"),
        s_lit("Authorization"),
        s_lit(f"Bearer {{{{secret:{secret_name}}}}}"),
        b_var("map_empty"),
    )


def _operation_authed(op, global_security):
    # Operation-level `security: []` disables auth; absent inherits the global requirement.
    sec = op.get("security", global_security)
    return bool(sec)


def build_operation(base_url, path, verb, op, global_security, secret_name):
    """Return a (record, body_ast) pair for one operation, or None if unsupported."""
    verb = verb.upper()
    op_id = op.get("operationId") or _param_name(f"{verb}_{path}")
    params = [p for p in op.get("parameters", []) if p.get("in") == "path"]
    path_param_names = [_param_name(p["name"]) for p in params]
    has_body = "requestBody" in op

    lam_params = ["base"] + path_param_names + (["body"] if has_body else [])
    for name in lam_params:
        if lam_params.count(name) > 1:
            return None  # a name collision (e.g. a path param literally called "base") — skip honestly

    url = url_builder(b_var("base"), path)
    headers = auth_header_map(secret_name) if _operation_authed(op, global_security) else b_var("map_empty")
    body_arg = b_var("body") if has_body else s_lit("")
    call = curried_app(b_var("http"), s_lit(verb), url, headers, body_arg)
    body_ast = {"kind": "lambda",
                "params": [{"name": p} for p in lam_params],
                "body": b_field(call, "status")}

    param_types = [STRING] * len(lam_params)
    type_ast = {"kind": "fn", "params": param_types, "result": INT}
    effect = "net.read" if verb in _READ_VERBS else "net.write"

    example = _example_for(base_url, verb, path_param_names, has_body, op)
    intent = ["io", "io/network/http"] + (["query/lookup"] if verb in _READ_VERBS else [])

    record = build_v2_record(
        name=op_id, type_ast=type_ast, examples=[example], body_text=body_ast,
        module_name=None, extra_hints=[_param_name(op_id)],
        effects=[effect], terminates="always", intent_tags=intent, complexity="O(n)",
    )
    return record, body_ast


def _example_for(base_url, verb, path_param_names, has_body, op):
    """One worked example whose expected status is a DOCUMENTED, deterministic outcome: a read/delete
    on a guaranteed-absent name is the documented 404; a create is the documented 2xx; a bodyless
    probe is its single documented status. Verified live with `--verify-against`."""
    codes = sorted(int(c) for c in op.get("responses", {}) if c.isdigit())
    args = [{"kind": "string", "value": base_url}]
    # A name no test writes, so GET/DELETE are the documented absent-case; PUT/POST create it fresh.
    absent = "gw7-absent-x"
    for _ in path_param_names:
        args.append({"kind": "string", "value": absent if verb in ("GET", "DELETE") else "gw7-new"})
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
    base_url = (spec.get("servers") or [{}])[0].get("url", "http://localhost")
    global_security = spec.get("security", [])
    out = []
    for path, item in spec.get("paths", {}).items():
        for verb, op in item.items():
            if verb.lower() not in ("get", "put", "post", "delete", "head", "patch"):
                continue
            built = build_operation(base_url, path, verb, op, global_security, secret_name)
            if built is not None:
                out.append(built)
    return out


def certify(record_path, body_path, out_dir):
    r = subprocess.run(
        [_VALIDATOR, "certify", record_path, "--body", body_path, "--records", out_dir],
        capture_output=True, text=True,
    )
    return r.returncode == 0, r.stdout.strip().splitlines()[-1] if r.stdout else r.stderr.strip()


def verify_examples(record_path, out_dir, base_url, secret_name, token):
    host = re.sub(r"^https?://", "", base_url).split("/")[0].split(":")[0]
    r = subprocess.run(
        [_VALIDATOR, "run", record_path, "--records", out_dir,
         "--secret", f"{secret_name}={token}"],
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
    secret_name = args.secret_name
    if secret_name is None:
        schemes = (spec.get("components") or {}).get("securitySchemes") or {}
        secret_name = next(iter(schemes), "api_token")
    os.makedirs(args.out, exist_ok=True)

    ops = walk(spec, secret_name)
    if not ops:
        sys.exit("no supported operations found in the spec")

    # Write records + bodies first (so certify can resolve body_hash via --records), then gate.
    written = []
    for record, body_ast in ops:
        name = record["name_hints"][0]
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
            run_ok, run_msg = verify_examples(rp, args.out, args.verify_against, secret_name, args.token)
            line += f"  examples={'PASS' if run_ok else 'FAIL: ' + run_msg}"
            ok = ok and run_ok
        ok = ok and cert_ok
        print(line)
        if not cert_ok:
            print(f"    {cert_msg}")

    print(f"\nwrote {len(written)} records -> {args.out}  (secret placeholder: {{{{secret:{secret_name}}}}})")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
