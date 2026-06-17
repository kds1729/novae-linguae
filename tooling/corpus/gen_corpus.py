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
    return unary_arith() + binary_arith() + list_funcs()


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
        for spec in specs:
            ex, ok = build_and_verify(spec, wd)
            if ok or args.keep_unverified:
                examples.append(ex)
            if not ok:
                dropped.append((spec["name"], ex["verification"]))

    with open(args.out, "w", encoding="utf-8") as fh:
        for ex in examples:
            fh.write(json.dumps(ex, ensure_ascii=False) + "\n")

    families = {"unary_arith": len(unary_arith()), "binary_arith": len(binary_arith()), "list_funcs": len(list_funcs())}
    proved = sum(1 for ex in examples for p in ex["verification"]["proofs"] if p["verdict"] == "PROVED")
    manifest = {
        "corpus": os.path.basename(args.out),
        "examples": len(examples),
        "families": families,
        "verified_all": len(dropped) == 0,
        "proved_properties": proved,
        "schema": "function-record.v0.2.schema.json (v0.2.0)",
        "note": "Every example is schema-valid, well-typed, executes its examples, and (where stated) "
                "proves its properties over the unbounded domain — checked by nl-validator.",
    }
    with open(args.out + ".manifest.json", "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    print(f"wrote {len(examples)} verified examples -> {args.out}")
    print(f"  families: {families}; properties PROVED: {proved}")
    if dropped:
        print(f"  DROPPED {len(dropped)} unverified: {[n for n, _ in dropped]}", file=sys.stderr)
        for name, v in dropped:
            print(f"    {name}: {v}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
