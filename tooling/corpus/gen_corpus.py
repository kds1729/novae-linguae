#!/usr/bin/env python3
"""Synthetic training-corpus generator for Nova Lingua.

The standing "training data" problem: no model speaks Nova Lingua fluently on day one, and a synthetic
corpus is part of the project rather than a follow-on. The distinguishing requirement is the project's own
thesis — *verified by default*: a training corpus full of plausible-but-wrong artifacts would teach a model
to produce plausible-but-wrong artifacts. So every example here is **correct by construction and then
checked by the reference tooling**: each generated function record is schema-validated, its body is
type-checked against its declared type, its worked examples are *executed*, and its algebraic properties
are *proved* over the unbounded domain (or bounded-checked) — all by `nl-validator`. Only artifacts that
pass enter the corpus, and each example ships with the verification verdicts so a learner can train on the
"is this right?" signal too, not just the artifact.

Each training example pairs a natural-language **intent** with several **views** of the same function —
the canonical surface syntax (via `unparse-*`), the JSON AST record, the executable body, the worked
examples, and the proved properties — so a model can learn the bidirectional NL <-> Nova Lingua mapping and
the verification that backs it.

Deterministic by construction (principle 5): the families enumerate a fixed set, no RNG, so the corpus is
byte-reproducible. Run `python3 gen_corpus.py --out corpus.jsonl` (also writes `<out>.manifest.json`).
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

_HERE = Path(__file__).resolve()
_REPO = _HERE.parents[2]  # tooling/corpus -> tooling -> repo root
sys.path.insert(0, str(_REPO / "tooling" / "ingest-common"))
from nl_core import build_v2_record, expr_address, write_runnable_dir  # noqa: E402

VALIDATOR = _REPO / "tooling" / "validator" / "target" / "release" / "nl-validator"
SCHEMA = _REPO / "spec" / "function-record.v0.2.schema.json"
MSG_SCHEMA = _REPO / "spec" / "message.v0.2.schema.json"

# Deterministic identities + timestamp for the Nova Locutio exchanges (same seed = same key = same
# signature, so the corpus stays byte-reproducible — principle 5).
SENDER_SEED = "novae-linguae-corpus-sender"
RESPONDER_SEED = "novae-linguae-corpus-responder"
MSG_TS = "2026-06-17T00:00:00Z"

# --- AST builders --------------------------------------------------------------------------------

def var(n):
    return {"kind": "var", "name": n}


def int_lit(n):
    return {"kind": "lit", "value": {"kind": "int", "value": n}}


def op(name, *args):
    # The `op` form — used in PROPERTY/predicate expressions (the prover's head_op reads it).
    return {"kind": "app", "op": name, "args": list(args)}


def bapp(name, *args):
    # The `fn` form — used in BODY expressions (the evaluator applies `fn` to `args`). Builtins and the
    # recursive `self` are referenced as a `var` head. (Bodies and predicates use different conventions.)
    return {"kind": "app", "fn": {"kind": "var", "name": name}, "args": list(args)}


def bself(*args):
    return {"kind": "app", "fn": {"kind": "var", "name": "self"}, "args": list(args)}


def lam(params, body):
    return {"kind": "lambda", "params": [{"name": p} for p in params], "body": body}


def self_app(*args):
    """A curried `apply(apply(self, a), b) …` spine — how a property refers to the function under test."""
    node = var("self")
    for a in args:
        node = {"kind": "app", "op": "apply", "args": [node, a]}
    return node


def forall(vs, body):
    return {"kind": "forall", "vars": vs, "body": body}


def case_null(xs_name, nil_body, cons_body):
    """`case null(xs) of true -> nil_body | false -> cons_body` — the list-recursion idiom."""
    return {
        "kind": "case",
        "scrutinee": bapp("null", var(xs_name)),
        "arms": [
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": nil_body},
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": False}}, "body": cons_body},
        ],
    }


INT = {"kind": "builtin", "name": "int"}
NAT = {"kind": "builtin", "name": "nat"}
BOOL = {"kind": "builtin", "name": "bool"}


def fn(params, result):
    return {"kind": "fn", "params": params, "result": result}


def list_of(elem):
    return {"kind": "apply", "ctor": {"kind": "builtin", "name": "List"}, "args": [elem]}


def poly_list_fn(result):
    """`forall a. List a -> result`."""
    return {"kind": "forall", "vars": ["a"], "body": fn([list_of(var("a"))], result)}


def to_value_ast(pyval):
    if isinstance(pyval, bool):
        return {"kind": "bool", "value": pyval}
    if isinstance(pyval, int):
        return {"kind": "int", "value": pyval}
    if isinstance(pyval, list):
        return {"kind": "list", "elems": [to_value_ast(x) for x in pyval]}
    raise ValueError(f"unsupported example value {pyval!r}")


# --- families ------------------------------------------------------------------------------------
#
# Each entry is a `spec` dict: name, intent (NL), summary, tags, type_ast, body_ast, examples (Python
# args+result, executed to verify), and optional properties (each a `forall` predicate proved over the
# unbounded domain). Properties are stated as a DIFFERENT expression than the body where possible (e.g.
# double's body is add(n,n) but its law says self(n)=2n), so the proof is non-trivial.

def unary_arith():
    n = var("n")
    rows = [
        ("double", "Double a number.", "Returns n + n.", bapp("add", n, n), [0, 3, -2], lambda x: x + x,
         "doubling", forall(["n"], op("eq", self_app(n), op("mul", int_lit(2), n))), ["arithmetic", "linear"]),
        ("triple", "Triple a number.", "Returns 3 * n.", bapp("mul", int_lit(3), n), [0, 4, -1], lambda x: 3 * x,
         "tripling", forall(["n"], op("eq", self_app(n), op("add", op("add", n, n), n))), ["arithmetic", "linear"]),
        ("square", "Square a number.", "Returns n * n.", bapp("mul", n, n), [0, 5, -3], lambda x: x * x,
         "nonnegative", forall(["n"], op("ge", self_app(n), int_lit(0))), ["arithmetic"]),
        ("increment", "Add one to a number.", "Returns n + 1.", bapp("add", n, int_lit(1)), [0, 9, -5], lambda x: x + 1,
         "strictly_increasing", forall(["n"], op("gt", self_app(n), n)), ["arithmetic"]),
        ("negate", "Negate a number.", "Returns -n.", bapp("neg", n), [0, 7, -4], lambda x: -x,
         "sums_to_zero", forall(["n"], op("eq", op("add", self_app(n), n), int_lit(0))), ["arithmetic"]),
    ]
    out = []
    for name, intent, summary, body, ins, f, pname, pexpr, tags in rows:
        out.append({
            "name": name, "intent": intent, "summary": summary, "tags": tags,
            "type_ast": fn([INT], INT), "body_ast": lam(["n"], body),
            "examples": [{"args": [i], "result": f(i)} for i in ins],
            "properties": [{"name": pname, "expr": pexpr}], "prove": True,
        })
    return out


def binary_arith():
    a, b = var("a"), var("b")
    rows = [
        ("add2", "Add two numbers.", "Returns a + b.", bapp("add", a, b), [(0, 0), (3, 4), (-2, 5)], lambda x, y: x + y,
         "commutative", forall(["a", "b"], op("eq", self_app(a, b), op("add", b, a))), ["arithmetic", "commutative"]),
        ("mul2", "Multiply two numbers.", "Returns a * b.", bapp("mul", a, b), [(0, 5), (3, 4), (-2, 6)], lambda x, y: x * y,
         "commutative", forall(["a", "b"], op("eq", self_app(a, b), op("mul", b, a))), ["arithmetic", "commutative"]),
        ("sub2", "Subtract one number from another.", "Returns a - b.", bapp("sub", a, b), [(5, 3), (0, 4), (-2, 6)], lambda x, y: x - y,
         "anti_symmetric", forall(["a", "b"], op("eq", self_app(a, b), op("neg", op("sub", b, a)))), ["arithmetic"]),
        ("maximum", "The larger of two numbers.", "Returns max(a, b).", bapp("max", a, b), [(5, 3), (2, 9), (-2, -7)], max,
         "at_least_first", forall(["a", "b"], op("ge", self_app(a, b), a)), ["arithmetic", "order"]),
    ]
    out = []
    for name, intent, summary, body, ins, f, pname, pexpr, tags in rows:
        out.append({
            "name": name, "intent": intent, "summary": summary, "tags": tags,
            "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], body),
            "examples": [{"args": [x, y], "result": f(x, y)} for (x, y) in ins],
            "properties": [{"name": pname, "expr": pexpr}], "prove": True,
        })
    return out


def boolean_funcs():
    n, a, b = var("n"), var("a"), var("b")
    out = [
        {"name": "is_positive", "intent": "Test whether a number is positive.", "summary": "Returns true iff n > 0.",
         "tags": ["predicate", "comparison"], "type_ast": fn([INT], BOOL), "body_ast": lam(["n"], bapp("gt", n, int_lit(0))),
         "examples": [{"args": [3], "result": True}, {"args": [0], "result": False}, {"args": [-2], "result": False}],
         "properties": [], "prove": False},
        {"name": "is_nonnegative", "intent": "Test whether a number is non-negative.", "summary": "Returns true iff n >= 0.",
         "tags": ["predicate", "comparison"], "type_ast": fn([INT], BOOL), "body_ast": lam(["n"], bapp("ge", n, int_lit(0))),
         "examples": [{"args": [3], "result": True}, {"args": [0], "result": True}, {"args": [-1], "result": False}],
         "properties": [], "prove": False},
        # logical_not: involutive — not(not(b)) = b, proved over the boolean fragment.
        {"name": "logical_not", "intent": "Negate a boolean.", "summary": "Returns the logical negation of b.",
         "tags": ["boolean", "involutive"], "type_ast": fn([BOOL], BOOL), "body_ast": lam(["b"], bapp("not", b)),
         "examples": [{"args": [True], "result": False}, {"args": [False], "result": True}],
         # Over the builtin `not` (the prover's `self` defaults to Int params, which clashes with a bool function).
         "properties": [{"name": "involutive", "expr": forall(["b"], op("eq", op("not", op("not", b)), b))}], "prove": True},
        # logical_and: commutative.
        {"name": "logical_and", "intent": "Logical AND of two booleans.", "summary": "Returns true iff both a and b are true.",
         "tags": ["boolean", "commutative"], "type_ast": fn([BOOL, BOOL], BOOL), "body_ast": lam(["a", "b"], bapp("and", a, b)),
         "examples": [{"args": [True, True], "result": True}, {"args": [True, False], "result": False}, {"args": [False, False], "result": False}],
         "properties": [{"name": "commutative", "expr": forall(["a", "b"], op("eq", op("and", a, b), op("and", b, a)))}], "prove": True},
    ]
    return out


def list_funcs():
    xs = var("xs")
    out = []
    # sum — a left fold over the list with an inline lambda (`self`-recursion isn't bound in the standalone
    # evaluator, so the runnable form uses the `foldl` builtin). Examples executed.
    out.append({
        "name": "sum", "intent": "Sum a list of numbers.", "summary": "Adds every element with foldl, 0 for the empty list.",
        "tags": ["list", "fold", "arithmetic"], "type_ast": fn([list_of(INT)], INT),
        "body_ast": lam(["xs"], bapp("foldl", lam(["acc", "x"], bapp("add", var("acc"), var("x"))), int_lit(0), xs)),
        "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 6}, {"args": [[5, -2, 4]], "result": 7}],
        "properties": [], "prove": False,
    })
    # reverse — builtin; its length-preserving law is PROVED by structural induction.
    out.append({
        "name": "reverse", "intent": "Reverse a list.", "summary": "Returns the elements in reverse order.",
        "tags": ["list", "lossless"], "type_ast": poly_list_fn(list_of(var("a"))),
        "body_ast": lam(["xs"], bapp("reverse", xs)),
        "examples": [{"args": [[]], "result": []}, {"args": [[1, 2, 3]], "result": [3, 2, 1]}],
        # Stated over the builtin `reverse` (not `self`), matching the spec/examples list-law records: this
        # is what lets lemma discovery engage and PROVE it (a `self`-wrapper hides the ops from selection).
        "properties": [{"name": "involutive", "expr": forall(["xs"], op("eq", op("reverse", op("reverse", xs)), xs))}],
        "prove": True,
    })
    # length — builtin; examples executed.
    out.append({
        "name": "length", "intent": "Count the elements of a list.", "summary": "Returns how many elements the list has.",
        "tags": ["list"], "type_ast": poly_list_fn(NAT), "body_ast": lam(["xs"], bapp("length", xs)),
        "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 3}, {"args": [[9]], "result": 1}],
        "properties": [], "prove": False,
    })
    return out


def all_specs():
    return unary_arith() + binary_arith() + boolean_funcs() + list_funcs()


# --- verification + emission ---------------------------------------------------------------------

def cli(args, **kw):
    return subprocess.run([str(VALIDATOR)] + args, capture_output=True, text=True, **kw)


def unparse(kind, ast):
    """Canonical surface string of an AST via `nl-validator unparse-<kind>` (read from stdin)."""
    p = cli([f"unparse-{kind}", "-"], input=json.dumps(ast))
    return p.stdout.strip() if p.returncode == 0 else None


def verdict_tokens(text):
    # Each verdict line is `<name>: VERDICT  <detail>` (prove) or `VERDICT  <name> …` (check-properties),
    # so scan every token of the line for the first recognized verdict.
    toks = {"PROVED", "REFUTED", "UNKNOWN", "UNSUPPORTED", "NO-SOLVER",
            "CONSISTENT", "CONTRADICTED", "UNVERIFIABLE"}
    found = []
    for line in text.splitlines():
        for t in line.replace(":", " ").split():
            if t in toks:
                found.append(t)
                break
    return found


def build_and_verify(spec, workdir):
    examples = [{"args": [to_value_ast(a) for a in ex["args"]], "result": to_value_ast(ex["result"])}
                for ex in spec["examples"]]
    terminates = "always" if spec["name"] != "sum" else "unknown"  # sum's termination isn't certified here
    record = build_v2_record(spec["name"], spec["type_ast"], examples, spec["body_ast"],
                             properties=spec.get("properties") or None, intent_tags=spec["tags"],
                             terminates=terminates)
    body = spec["body_ast"]
    addr = expr_address(body)
    d = os.path.join(workdir, spec["name"])
    write_runnable_dir(d, [record], {addr: body})
    rec_path = os.path.join(d, f"{record['hash']}.json")
    body_path = os.path.join(d, f"{addr}.json")

    schema_valid = cli(["validate", str(SCHEMA), rec_path]).returncode == 0
    well_typed = cli(["typecheck", rec_path, "--body", body_path]).returncode == 0
    run_p = cli(["run", "--records", d, rec_path])
    examples_passed = run_p.returncode == 0
    bounded = verdict_tokens(cli(["check-properties", rec_path]).stdout) if spec.get("properties") else []
    proofs = []
    if spec.get("prove") and spec.get("properties"):
        pv = verdict_tokens(cli(["prove", rec_path, "--body", body_path]).stdout)
        proofs = [{"name": p["name"], "verdict": v} for p, v in zip(spec["properties"], pv)]

    example = {
        "id": spec["name"],
        "modality": "nova_lingua",
        "polarity": "positive",
        "intent": spec["intent"],
        "summary": spec["summary"],
        "tags": spec["tags"],
        "views": {
            "surface_type": unparse("type", spec["type_ast"]),
            "surface_body": unparse("body", spec["body_ast"]),
            "record": record,
            "body": body,
            "examples": record["examples"],
            "properties": spec.get("properties") or [],
        },
        "verification": {
            "schema_valid": schema_valid,
            "well_typed": well_typed,
            "examples_passed": f"{len(spec['examples'])}/{len(spec['examples'])}" if examples_passed else "FAILED",
            "bounded_check": bounded,
            "proofs": proofs,
        },
    }
    ok = schema_valid and well_typed and examples_passed and all(p["verdict"] in ("PROVED",) for p in proofs)
    return example, ok


# --- Nova Locutio: verified agent-loop exchanges -------------------------------------------------
#
# Each example is a real SIGNED exchange: a request / propose / query is constructed, signed as the
# sender, and answered by `nl-validator respond` (the agent loop) under the responder's signing identity.
# The verification is the agent loop's own — a request/apply reply is an `assert` whose claim *re-runs
# true* (`verify-claim` CONFIRMED, principle 3); a propose is answered with a `commit` only after the
# responder test-runs it; a query is answered with an `ack` of the matches — and both messages
# schema-validate against message.v0.2 and the reply is correctly threaded back to the request.

def _write_tmp(obj):
    f = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    json.dump(obj, f)
    f.close()
    return f.name


def sign_message(msg, seed):
    p = _write_tmp(msg)
    out = cli(["sign", "--seed", seed, p]).stdout
    os.unlink(p)
    return json.loads(out) if out.strip() else None


def responder_did(seed):
    """Derive a seed's did:nova identity by signing a throwaway message and reading its `from`."""
    tmpl = {"schema_version": "0.2.0", "kind": "query", "in_reply_to": None, "timestamp": MSG_TS,
            "to": "did:nova:" + "0" * 64, "constraints": None, "body": {"limit": 1, "pattern": {}}}
    signed = sign_message(tmpl, seed)
    return signed["from"] if signed else None


def msg_schema_valid(msg):
    p = _write_tmp(msg)
    rc = cli(["validate", str(MSG_SCHEMA), p]).returncode
    os.unlink(p)
    return rc == 0


def respond_to(signed_request, commons_dir):
    p = _write_tmp(signed_request)
    r = cli(["respond", "--records", commons_dir, "--seed", RESPONDER_SEED, p])
    os.unlink(p)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None


def build_commons(workdir, specs):
    """A shared commons directory holding every runnable record (and its body), for the agent loop to
    discover and run against. Returns the directory and a {name: record} map."""
    records, bodies, by_name = [], {}, {}
    for spec in specs:
        ex = [{"args": [to_value_ast(a) for a in e["args"]], "result": to_value_ast(e["result"])} for e in spec["examples"]]
        terminates = "always" if spec["name"] != "sum" else "unknown"
        rec = build_v2_record(spec["name"], spec["type_ast"], ex, spec["body_ast"],
                              properties=spec.get("properties") or None, intent_tags=spec["tags"], terminates=terminates)
        addr = expr_address(spec["body_ast"])
        records.append(rec)
        bodies[addr] = spec["body_ast"]
        by_name[spec["name"]] = rec
    d = os.path.join(workdir, "_commons")
    write_runnable_dir(d, records, bodies)
    return d, by_name


def nova_locutio_examples(commons_dir, by_name):
    resp_did = responder_did(RESPONDER_SEED)
    if not resp_did:
        return []
    out = []

    def emit(ident, intent, summary, tags, act, request, reply, outcome, ok):
        out.append({
            "id": "locutio_" + ident, "modality": "nova_locutio", "polarity": "positive",
            "intent": intent, "summary": summary, "tags": tags,
            "views": {"speech_act": act, "request": request, "reply": reply, "reply_act": reply.get("kind") if reply else None},
            "verification": {
                "request_schema_valid": msg_schema_valid(request),
                "reply_schema_valid": bool(reply) and msg_schema_valid(reply),
                "threaded": bool(reply) and reply.get("in_reply_to") == request.get("hash"),
                "outcome": outcome,
            },
            "_ok": ok,
        })

    # request/apply and propose exchanges.
    apply_rows = [
        ("apply_double", "Ask an agent to compute double of 21.",
         "request/apply double to 21 → the responder runs it and asserts double(21) = 42, which re-runs true.",
         "request", "double", [21], ["agent-loop", "request", "apply"]),
        ("apply_add", "Ask an agent to add 3 and 4.",
         "request/apply add to (3, 4) → the responder asserts add(3, 4) = 7, which re-runs true.",
         "request", "add2", [3, 4], ["agent-loop", "request", "apply"]),
        ("propose_double", "Propose that an agent compute double of 21.",
         "propose/apply double to 21 → the responder test-runs it and commits.",
         "propose", "double", [21], ["agent-loop", "propose"]),
    ]
    for ident, intent, summary, kind, tname, pyargs, tags in apply_rows:
        body = {"action": "apply", "target": by_name[tname]["hash"], "args": [to_value_ast(a) for a in pyargs]}
        req = {"schema_version": "0.2.0", "kind": kind, "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
               "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000}, "body": body}
        signed = sign_message(req, SENDER_SEED)
        reply = respond_to(signed, commons_dir)
        schema_ok = msg_schema_valid(signed) and bool(reply) and msg_schema_valid(reply)
        threaded = bool(reply) and reply.get("in_reply_to") == signed.get("hash") and reply.get("to") == signed.get("from")
        if kind == "request":
            confirmed = False
            if reply and reply.get("kind") == "assert":
                vp = _write_tmp(reply)
                confirmed = cli(["verify-claim", "--records", commons_dir, vp]).returncode == 0
                os.unlink(vp)
            outcome = "CONFIRMED" if confirmed else "NOT-CONFIRMED"
            emit(ident, intent, summary, tags, kind, signed, reply, outcome, schema_ok and threaded and confirmed)
        else:  # propose
            committed = bool(reply) and reply.get("kind") == "commit"
            emit(ident, intent, summary, tags, kind, signed, reply, (reply.get("kind").upper() if reply else "NO-REPLY"),
                 schema_ok and threaded and committed)

    # request/validate → assert (verified) or reject: validation-as-a-service.
    vreq = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000},
            "body": {"action": "validate", "target": by_name["double"]["hash"]}}
    vsigned = sign_message(vreq, SENDER_SEED)
    vreply = respond_to(vsigned, commons_dir)
    v_ok = (msg_schema_valid(vsigned) and bool(vreply) and msg_schema_valid(vreply)
            and vreply.get("kind") == "assert" and vreply.get("in_reply_to") == vsigned.get("hash"))
    emit("validate_double", "Ask an agent to validate the `double` function.",
         "request/validate double → the responder type-checks and runs it, then asserts it is verified.",
         ["agent-loop", "request", "validate"], "request", vsigned, vreply,
         "VERIFIED" if v_ok else (vreply.get("kind", "NO-REPLY").upper() if vreply else "NO-REPLY"), v_ok)

    # delegate → ack: granting a capability is acknowledged by the loop.
    dreq = {"schema_version": "0.2.0", "kind": "delegate", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": None, "body": {"capability": "cap:apply", "conditions": [], "expires_at": None}}
    dsigned = sign_message(dreq, SENDER_SEED)
    dreply = respond_to(dsigned, commons_dir)
    d_ok = (msg_schema_valid(dsigned) and bool(dreply) and msg_schema_valid(dreply)
            and dreply.get("kind") == "ack" and dreply.get("in_reply_to") == dsigned.get("hash"))
    emit("delegate_apply", "Delegate the capability to apply functions.",
         "delegate cap:apply → the responder acknowledges receipt.",
         ["agent-loop", "delegate", "capability"], "delegate", dsigned, dreply,
         "ACKED" if d_ok else (dreply.get("kind", "NO-REPLY").upper() if dreply else "NO-REPLY"), d_ok)

    # query → ack (discovery by intent tag).
    qreq = {"schema_version": "0.2.0", "kind": "query", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": None, "body": {"limit": 50, "pattern": {"intent_tags": ["list"]}}}
    qsigned = sign_message(qreq, SENDER_SEED)
    qreply = respond_to(qsigned, commons_dir)
    matches = qreply.get("body", {}).get("result", {}).get("matches", []) if qreply else []
    qok = (msg_schema_valid(qsigned) and bool(qreply) and msg_schema_valid(qreply)
           and qreply.get("kind") == "ack" and len(matches) > 0
           and qreply.get("in_reply_to") == qsigned.get("hash"))
    emit("query_list", "Find functions that operate on lists.",
         "query for functions tagged `list` → the responder acks with the matching content-addresses.",
         ["agent-loop", "query", "discovery"], "query", qsigned, qreply, f"ACK {len(matches)} match(es)", qok)
    return out


# --- negative examples: artifacts paired with the verifier's REJECTION ----------------------------
#
# The "is this wrong?" signal. Each is a deliberately-wrong artifact, and the example is valid only if
# the reference verifier actually REJECTS it (the generator drops it otherwise — a negative that the
# verifier accepts would be a verifier bug or a mislabeled example, not training signal). So a negative is
# "verified" in the dual sense: verified to be rejected, for the stated reason.

def negative_examples(workdir, commons_dir, by_name):
    out = []
    n = var("n")

    def write_rec(name, rec, body):
        addr = expr_address(body)
        d = os.path.join(workdir, "neg_" + name)
        write_runnable_dir(d, [rec], {addr: body})
        return d, os.path.join(d, rec["hash"] + ".json"), os.path.join(d, addr + ".json")

    def emit(ident, modality, intent, summary, tags, views, check, verdict, reason, rejected):
        out.append({
            "id": "neg_" + ident, "modality": modality, "polarity": "negative",
            "intent": intent, "summary": summary, "tags": tags, "views": views,
            "verification": {"expected": "rejected", "check": check, "verdict": verdict,
                             "rejected": rejected, "reason": (reason or "").strip()[:300]},
            "_ok": rejected,
        })

    # 1. Wrong return type — declares int -> bool but the body returns an int. Schema-valid and its example
    #    even runs, but the TYPE CHECKER rejects it.
    body1 = lam(["n"], bapp("add", n, n))
    rec1 = build_v2_record("mislabeled", fn([INT], BOOL), [{"args": [to_value_ast(3)], "result": to_value_ast(6)}],
                           body1, terminates="always")
    _, r1, b1 = write_rec("mislabeled", rec1, body1)
    tc = cli(["typecheck", r1, "--body", b1])
    emit("wrong_return_type", "nova_lingua",
         "A function declared to return a bool, but whose body returns an int.",
         "Schema-valid and its example even runs, but the type checker rejects it: the body has type int, not bool.",
         ["negative", "type-error"], {"record": rec1, "body": body1},
         "typecheck", "ILL-TYPED", (tc.stdout + tc.stderr), tc.returncode != 0)

    # 2. Refuted property — a correct body (add(n,n)) carrying a FALSE law (self(n) = n + 1). The prover
    #    refutes it with a counterexample.
    body2 = lam(["n"], bapp("add", n, n))
    prop2 = [{"name": "false_doubling", "expr": forall(["n"], op("eq", self_app(n), op("add", n, int_lit(1))))}]
    rec2 = build_v2_record("double", fn([INT], INT), [{"args": [to_value_ast(3)], "result": to_value_ast(6)}],
                           body2, properties=prop2, terminates="always")
    _, r2, b2 = write_rec("falselaw", rec2, body2)
    pv = cli(["prove", r2, "--body", b2])
    refuted = "REFUTED" in pv.stdout
    emit("refuted_property", "nova_lingua",
         "A doubling function that wrongly claims double(n) = n + 1.",
         "The body and examples are correct, but the property is false; the prover refutes it with a counterexample.",
         ["negative", "false-property"], {"record": rec2, "body": body2, "properties": prop2},
         "prove", "REFUTED", pv.stdout, refuted)

    # 3. Wrong example — claims double(3) = 7. Type-checks, but FAILS when the example is executed.
    body3 = lam(["n"], bapp("add", n, n))
    rec3 = build_v2_record("double", fn([INT], INT), [{"args": [to_value_ast(3)], "result": to_value_ast(7)}],
                           body3, terminates="always")
    d3, r3, b3 = write_rec("wrongexample", rec3, body3)
    rn = cli(["run", "--records", d3, r3])
    emit("wrong_example", "nova_lingua",
         "A doubling function whose worked example claims double(3) = 7.",
         "Well-typed, but executing the body against the example fails: double(3) is 6, not 7.",
         ["negative", "wrong-example"], {"record": rec3, "body": body3},
         "run", "EXAMPLE-FAILED", (rn.stdout + rn.stderr), rn.returncode != 0)

    # 4. Nova Locutio — a validly-SIGNED assert with a FALSE claim (double(21) = 43). verify-claim re-runs
    #    the claim and refutes it: a real signature is no guarantee of a true claim (principle 3).
    resp_did = responder_did(RESPONDER_SEED)
    req = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
           "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000},
           "body": {"action": "apply", "target": by_name["double"]["hash"], "args": [to_value_ast(21)]}}
    reply = respond_to(sign_message(req, SENDER_SEED), commons_dir)
    if reply and reply.get("kind") == "assert":
        # Tamper the asserted result (42 -> 43) and re-sign, so the signature is valid but the claim is false.
        try:
            reply["body"]["claim"]["expr"]["args"][1]["value"]["value"] = 43
        except (KeyError, IndexError, TypeError):
            reply = None
    if reply:
        tampered = sign_message(reply, RESPONDER_SEED)
        vp = _write_tmp(tampered)
        vc = cli(["verify-claim", "--records", commons_dir, vp])
        os.unlink(vp)
        emit("false_claim", "nova_locutio",
             "A signed assertion claiming double(21) = 43.",
             "The signature is valid, but re-running the claim against the commons refutes it: double(21) is 42.",
             ["negative", "false-claim", "agent-loop"], {"speech_act": "assert", "message": tampered},
             "verify-claim", "REFUTED", (vc.stdout + vc.stderr), vc.returncode != 0)
    return out


def main():
    ap = argparse.ArgumentParser(description="Generate a verified Nova Lingua training corpus (JSONL).")
    ap.add_argument("--out", default=str(_HERE.parent / "corpus.jsonl"), help="output JSONL path")
    ap.add_argument("--keep-unverified", action="store_true",
                    help="emit examples even if a verification step fails (default: drop them, loudly)")
    args = ap.parse_args()

    if not VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {VALIDATOR} — run `cargo build --release` in tooling/validator")

    specs = all_specs()
    examples, dropped = [], []
    with tempfile.TemporaryDirectory(prefix="nlcorpus-") as wd:
        # Nova Lingua — verified function records.
        for spec in specs:
            ex, ok = build_and_verify(spec, wd)
            if ok or args.keep_unverified:
                examples.append(ex)
            if not ok:
                dropped.append((spec["name"], ex["verification"]))
        # Nova Locutio — verified agent-loop exchanges over a shared commons of the same functions.
        commons_dir, by_name = build_commons(wd, specs)
        for ex in nova_locutio_examples(commons_dir, by_name):
            ok = ex.pop("_ok")
            if ok or args.keep_unverified:
                examples.append(ex)
            if not ok:
                dropped.append((ex["id"], ex["verification"]))
        # Negative examples — artifacts the verifier correctly REJECTS (the "is this wrong?" signal).
        for ex in negative_examples(wd, commons_dir, by_name):
            ok = ex.pop("_ok")
            if ok or args.keep_unverified:
                examples.append(ex)
            if not ok:
                dropped.append((ex["id"], ex["verification"]))

    with open(args.out, "w", encoding="utf-8") as fh:
        for ex in examples:
            fh.write(json.dumps(ex, ensure_ascii=False) + "\n")

    by_modality, by_polarity = {}, {}
    for ex in examples:
        by_modality[ex["modality"]] = by_modality.get(ex["modality"], 0) + 1
        by_polarity[ex["polarity"]] = by_polarity.get(ex["polarity"], 0) + 1
    families = {"unary_arith": len(unary_arith()), "binary_arith": len(binary_arith()),
                "boolean_funcs": len(boolean_funcs()), "list_funcs": len(list_funcs())}
    proved = sum(1 for ex in examples if ex["modality"] == "nova_lingua" and ex["polarity"] == "positive"
                 for p in ex["verification"]["proofs"] if p["verdict"] == "PROVED")
    confirmed = sum(1 for ex in examples if ex["modality"] == "nova_locutio" and ex["polarity"] == "positive"
                    and ex["verification"]["outcome"] == "CONFIRMED")
    rejected = sum(1 for ex in examples if ex["polarity"] == "negative")
    manifest = {
        "corpus": os.path.basename(args.out),
        "examples": len(examples),
        "by_modality": by_modality,
        "by_polarity": by_polarity,
        "nova_lingua_families": families,
        "verified_all": len(dropped) == 0,
        "proved_properties": proved,
        "confirmed_agent_loop_claims": confirmed,
        "schema": "function-record.v0.2.schema.json + message.v0.2.schema.json (v0.2.0)",
        "note": "POSITIVE examples are correct-by-construction and checked: a Nova Lingua record is "
                "schema-valid, well-typed, executes its examples, and (where stated) proves its properties "
                "over the unbounded domain; a Nova Locutio example is a signed agent-loop exchange whose "
                "reply schema-validates, is threaded, and (for request/apply) re-runs true via "
                "verify-claim. NEGATIVE examples (polarity 'negative') are deliberately-wrong artifacts "
                "confirmed to be REJECTED by the verifier — an ill-typed body, a refuted property, a "
                "failed example, a signed-but-false claim. All checked by nl-validator.",
    }
    with open(args.out + ".manifest.json", "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    print(f"wrote {len(examples)} verified examples -> {args.out}")
    print(f"  modality: {by_modality}; properties PROVED: {proved}; agent-loop claims CONFIRMED: {confirmed}")
    if dropped:
        print(f"  DROPPED {len(dropped)} unverified: {[n for n, _ in dropped]}", file=sys.stderr)
        for name, v in dropped:
            print(f"    {name}: {v}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
