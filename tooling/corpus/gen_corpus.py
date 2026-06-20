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
from nl_core import build_v2_record, content_hash, expr_address, write_runnable_dir  # noqa: E402

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


def bool_lit(b):
    return {"kind": "lit", "value": {"kind": "bool", "value": b}}


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


def case_bool(scrutinee, true_body, false_body):
    """`case <scrutinee> of true -> true_body | false -> false_body` — a boolean two-way branch."""
    return {
        "kind": "case",
        "scrutinee": scrutinee,
        "arms": [
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": true_body},
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": False}}, "body": false_body},
        ],
    }


def variant_expr(tag, payload=None):
    """A variant-construction BODY expression: `None` (nullary) or `Just(<payload expr>)`."""
    v = {"kind": "variant", "tag": tag}
    if payload is not None:
        v["payload"] = payload
    return v


def maybe_t(elem):
    """The sum type `[Just(elem) None]`."""
    return {"kind": "sum", "variants": [{"tag": "Just", "type": elem}, {"tag": "None"}]}


def result_t(ok_t, err_t):
    """The sum type `[Ok(ok_t) Err(err_t)]`."""
    return {"kind": "sum", "variants": [{"tag": "Ok", "type": ok_t}, {"tag": "Err", "type": err_t}]}


INT = {"kind": "builtin", "name": "int"}
NAT = {"kind": "builtin", "name": "nat"}
BOOL = {"kind": "builtin", "name": "bool"}
FLOAT = {"kind": "builtin", "name": "float"}


def fn(params, result):
    return {"kind": "fn", "params": params, "result": result}


def list_of(elem):
    return {"kind": "apply", "ctor": {"kind": "builtin", "name": "List"}, "args": [elem]}


def poly_list_fn(result):
    """`forall a. List a -> result`."""
    return {"kind": "forall", "vars": ["a"], "body": fn([list_of(var("a"))], result)}


_NO_PAYLOAD = object()  # sentinel distinguishing a nullary variant from one with a `0`/falsey payload


class V:
    """A variant value for worked examples: `V("Just", 3)` or the nullary `V("None")`."""

    def __init__(self, tag, payload=_NO_PAYLOAD):
        self.tag = tag
        self.payload = payload


def to_value_ast(pyval):
    if isinstance(pyval, bool):
        return {"kind": "bool", "value": pyval}
    if isinstance(pyval, int):
        return {"kind": "int", "value": pyval}
    if isinstance(pyval, float):
        return {"kind": "float", "value": pyval}
    if isinstance(pyval, list):
        return {"kind": "list", "elems": [to_value_ast(x) for x in pyval]}
    if isinstance(pyval, V):
        out = {"kind": "variant", "tag": pyval.tag}
        if pyval.payload is not _NO_PAYLOAD:
            out["payload"] = to_value_ast(pyval.payload)
        return out
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
        ("quadruple", "Quadruple a number.", "Returns 4 * n.", bapp("mul", int_lit(4), n), [0, 3, -2], lambda x: 4 * x,
         "two_doublings", forall(["n"], op("eq", self_app(n), op("mul", int_lit(2), op("mul", int_lit(2), n)))), ["arithmetic", "linear"]),
        ("decrement", "Subtract one from a number.", "Returns n - 1.", bapp("sub", n, int_lit(1)), [0, 9, -5], lambda x: x - 1,
         "strictly_decreasing", forall(["n"], op("lt", self_app(n), n)), ["arithmetic"]),
        ("abs_val", "Absolute value of a number.", "Returns |n|.", bapp("abs", n), [0, 6, -4], abs,
         "nonnegative", forall(["n"], op("ge", self_app(n), int_lit(0))), ["arithmetic"]),
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
        ("minimum", "The smaller of two numbers.", "Returns min(a, b).", bapp("min", a, b), [(5, 3), (2, 9), (-2, -7)], min,
         "at_most_first", forall(["a", "b"], op("le", self_app(a, b), a)), ["arithmetic", "order"]),
        ("abs_diff", "The absolute difference of two numbers.", "Returns |a - b|.", bapp("abs", bapp("sub", a, b)),
         [(5, 3), (3, 5), (-2, 4)], lambda x, y: abs(x - y),
         "symmetric", forall(["a", "b"], op("eq", self_app(a, b), self_app(b, a))), ["arithmetic"]),
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
        # logical_or: commutative (over the boolean fragment).
        {"name": "logical_or", "intent": "Logical OR of two booleans.", "summary": "Returns true iff a or b is true.",
         "tags": ["boolean", "commutative"], "type_ast": fn([BOOL, BOOL], BOOL), "body_ast": lam(["a", "b"], bapp("or", a, b)),
         "examples": [{"args": [False, False], "result": False}, {"args": [True, False], "result": True}, {"args": [True, True], "result": True}],
         "properties": [{"name": "commutative", "expr": forall(["a", "b"], op("eq", op("or", a, b), op("or", b, a)))}], "prove": True},
        # is_zero: a predicate; examples only.
        {"name": "is_zero", "intent": "Test whether a number is zero.", "summary": "Returns true iff n == 0.",
         "tags": ["predicate", "comparison"], "type_ast": fn([INT], BOOL), "body_ast": lam(["n"], bapp("eq", n, int_lit(0))),
         "examples": [{"args": [0], "result": True}, {"args": [3], "result": False}, {"args": [-1], "result": False}],
         "properties": [], "prove": False},
        # logical_xor: commutative (over the boolean fragment).
        {"name": "logical_xor", "intent": "Exclusive OR of two booleans.", "summary": "Returns true iff exactly one of a, b is true.",
         "tags": ["boolean", "commutative"], "type_ast": fn([BOOL, BOOL], BOOL), "body_ast": lam(["a", "b"], bapp("xor", a, b)),
         "examples": [{"args": [False, False], "result": False}, {"args": [True, False], "result": True}, {"args": [True, True], "result": False}],
         "properties": [{"name": "commutative", "expr": forall(["a", "b"], op("eq", op("xor", a, b), op("xor", b, a)))}], "prove": True},
        # is_even: a predicate over n mod 2; examples only.
        {"name": "is_even", "intent": "Test whether a number is even.", "summary": "Returns true iff n mod 2 == 0.",
         "tags": ["predicate", "arithmetic"], "type_ast": fn([INT], BOOL),
         "body_ast": lam(["n"], bapp("eq", bapp("mod", n, int_lit(2)), int_lit(0))),
         "examples": [{"args": [0], "result": True}, {"args": [4], "result": True}, {"args": [3], "result": False}],
         "properties": [], "prove": False},
    ]
    return out


def list_funcs():
    xs = var("xs")
    out = []
    # sum — a left fold over the list with an inline lambda. (The `recursive_funcs` family carries the
    # raw `self`-recursive form, `sum_rec`; this one shows the fold idiom for the same intent.) Examples executed.
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
        # The second law — reverse commutes with filtering — is the headline of the auxiliary-lemma
        # frontier: its induction needs `filter_append` (and `append_nil`), which the prover now isolates
        # from the rest of the catalog (an explicit e-matching trigger + minimal-subset search) so z3's
        # quantifier instantiation doesn't stall. `p` is a quantified predicate, modelled uninterpreted.
        "properties": [
            {"name": "involutive", "expr": forall(["xs"], op("eq", op("reverse", op("reverse", xs)), xs))},
            {"name": "commutes_with_filter",
             "expr": forall(["p", "xs"], op("eq", op("filter", var("p"), op("reverse", xs)),
                                            op("reverse", op("filter", var("p"), xs))))},
            # Length-preserving — proved by induction, discovering the `length_append` lemma.
            {"name": "length_preserving", "expr": forall(["xs"], op("eq", op("length", op("reverse", xs)), op("length", xs)))},
        ],
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


def list_transform_funcs():
    xs, ys, x = var("xs"), var("ys"), var("x")
    return [
        # map with an inline lambda — runnable (the evaluator applies the closure).
        {"name": "negate_all", "intent": "Negate every number in a list.", "summary": "Maps neg over the list.",
         "tags": ["list", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("map", lam(["x"], bapp("neg", x)), xs)),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [-1, 2, -3]}],
         # Mapping preserves length, for EVERY function — proved with `f` modelled uninterpreted. Stated
         # generically over `f`, independent of this record's concrete `neg`.
         "properties": [{"name": "map_length",
                         "expr": forall(["f", "xs"], op("eq", op("length", op("map", var("f"), xs)), op("length", xs)))}],
         "prove": True},
        # filter with an inline predicate.
        {"name": "keep_positives", "intent": "Keep only the positive numbers in a list.",
         "summary": "Filters the list to its positive elements.", "tags": ["list", "filter"],
         "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("filter", lam(["x"], bapp("gt", x, int_lit(0))), xs)),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3, 0]], "result": [1, 3]}],
         # Filtering is idempotent — proved by direct induction (no auxiliary lemma). Stated generically
         # over the predicate `p` (modelled uninterpreted), independent of this record's concrete `> 0`.
         "properties": [{"name": "idempotent",
                         "expr": forall(["p", "xs"], op("eq", op("filter", var("p"), op("filter", var("p"), xs)),
                                                       op("filter", var("p"), xs)))}],
         "prove": True},
        # append — binary; length is additive (proved over the builtins via the length_append lemma).
        {"name": "concat", "intent": "Concatenate two lists.", "summary": "Appends the second list onto the first.",
         "tags": ["list", "lossless"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([list_of(var("a")), list_of(var("a"))], list_of(var("a")))},
         "body_ast": lam(["xs", "ys"], bapp("append", xs, ys)),
         "examples": [{"args": [[], [1]], "result": [1]}, {"args": [[1, 2], [3, 4]], "result": [1, 2, 3, 4]}],
         "properties": [{"name": "length_additive",
                         "expr": forall(["xs", "ys"], op("eq", op("length", op("append", xs, ys)),
                                                       op("add", op("length", xs), op("length", ys))))}],
         "prove": True},
        # map with a squaring lambda.
        {"name": "square_all", "intent": "Square every number in a list.", "summary": "Maps n*n over the list.",
         "tags": ["list", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("map", lam(["x"], bapp("mul", x, x)), xs)),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [1, 4, 9]}],
         "properties": [], "prove": False},
        # filter with an evenness predicate.
        {"name": "keep_evens", "intent": "Keep only the even numbers in a list.",
         "summary": "Filters the list to its even elements.", "tags": ["list", "filter"],
         "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0))), xs)),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, 2, 3, 4]], "result": [2, 4]}],
         "properties": [], "prove": False},
    ]


def composition_funcs():
    xs, x = var("xs"), var("x")
    return [
        # product — a left fold with multiplication.
        {"name": "product", "intent": "Multiply all the numbers in a list.",
         "summary": "Folds mul over the list, 1 for the empty list.", "tags": ["list", "fold", "arithmetic"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", lam(["acc", "x"], bapp("mul", var("acc"), x)), int_lit(1), xs)),
         "examples": [{"args": [[]], "result": 1}, {"args": [[2, 3, 4]], "result": 24}, {"args": [[5, -2]], "result": -10}],
         "properties": [], "prove": False},
        # count_positives — length . filter, a composition of two list builtins.
        {"name": "count_positives", "intent": "Count the positive numbers in a list.",
         "summary": "The length of the list's positive elements.", "tags": ["list", "filter", "composition"],
         "type_ast": fn([list_of(INT)], NAT),
         "body_ast": lam(["xs"], bapp("length", bapp("filter", lam(["x"], bapp("gt", x, int_lit(0))), xs))),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, -2, 3, 0, 4]], "result": 3}],
         "properties": [], "prove": False},
        # count_evens — length . filter(even).
        {"name": "count_evens", "intent": "Count the even numbers in a list.",
         "summary": "The length of the list's even elements.", "tags": ["list", "filter", "composition"],
         "type_ast": fn([list_of(INT)], NAT),
         "body_ast": lam(["xs"], bapp("length", bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0))), xs))),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3, 4]], "result": 2}],
         "properties": [], "prove": False},
        # sum_of_squares — foldl-add over the squares (a map fused into the fold).
        {"name": "sum_of_squares", "intent": "Sum the squares of a list of numbers.",
         "summary": "Folds add over each element squared, 0 for the empty list.", "tags": ["list", "map", "fold", "composition"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", lam(["acc", "x"], bapp("add", var("acc"), bapp("mul", x, x))), int_lit(0), xs)),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 14}, {"args": [[2, -3]], "result": 13}],
         "properties": [], "prove": False},
    ]


def list_fold_funcs():
    # Right-fold aggregations over a list — the `foldr` idiom (a complement to the `foldl` examples in
    # `list_funcs`/`composition_funcs`) and the corpus's first `List -> bool` functions (every other
    # boolean function takes a scalar). A predicate folds a per-element test with the boolean accumulator;
    # the empty list returns the fold's identity (`true` for "all", `false` for "any"/"contains"). Runnable;
    # examples only — these widen the vocabulary a model must read and write, not the proof surface.
    xs, x, acc = var("xs"), var("x"), var("acc")

    def foldr_body(step, init):
        return lam(["xs"], bapp("foldr", lam(["x", "acc"], step), init, xs))

    return [
        {"name": "all_positive", "intent": "Test whether every number in a list is positive.",
         "summary": "true for the empty list; otherwise (head > 0) and the rest are all positive.",
         "tags": ["list", "fold", "predicate"], "type_ast": fn([list_of(INT)], BOOL),
         "body_ast": foldr_body(bapp("and", bapp("gt", x, int_lit(0)), acc), bool_lit(True)),
         "examples": [{"args": [[]], "result": True}, {"args": [[1, 2, 3]], "result": True},
                      {"args": [[1, -2, 3]], "result": False}],
         "properties": [], "prove": False},
        {"name": "any_negative", "intent": "Test whether a list contains a negative number.",
         "summary": "false for the empty list; otherwise (head < 0) or the rest contains a negative.",
         "tags": ["list", "fold", "predicate"], "type_ast": fn([list_of(INT)], BOOL),
         "body_ast": foldr_body(bapp("or", bapp("lt", x, int_lit(0)), acc), bool_lit(False)),
         "examples": [{"args": [[]], "result": False}, {"args": [[1, 2, 3]], "result": False},
                      {"args": [[1, -2, 3]], "result": True}],
         "properties": [], "prove": False},
        {"name": "contains_zero", "intent": "Test whether a list contains a zero.",
         "summary": "false for the empty list; otherwise (head == 0) or the rest contains a zero.",
         "tags": ["list", "fold", "predicate"], "type_ast": fn([list_of(INT)], BOOL),
         "body_ast": foldr_body(bapp("or", bapp("eq", x, int_lit(0)), acc), bool_lit(False)),
         "examples": [{"args": [[]], "result": False}, {"args": [[1, 0, 2]], "result": True},
                      {"args": [[1, 2, 3]], "result": False}],
         "properties": [], "prove": False},
        {"name": "all_even", "intent": "Test whether every number in a list is even.",
         "summary": "true for the empty list; otherwise (head even) and the rest are all even.",
         "tags": ["list", "fold", "predicate", "arithmetic"], "type_ast": fn([list_of(INT)], BOOL),
         "body_ast": foldr_body(bapp("and", bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0)), acc), bool_lit(True)),
         "examples": [{"args": [[]], "result": True}, {"args": [[2, 4, 6]], "result": True},
                      {"args": [[1, 2, 3]], "result": False}],
         "properties": [], "prove": False},
        {"name": "sum_foldr", "intent": "Sum a list of numbers with a right fold.",
         "summary": "0 for the empty list; otherwise head + the sum of the rest.",
         "tags": ["list", "fold", "arithmetic"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": foldr_body(bapp("add", x, acc), int_lit(0)),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 6},
                      {"args": [[5, -2, 4]], "result": 7}],
         "properties": [], "prove": False},
    ]


def float_funcs():
    x = var("x")
    return [
        {"name": "square_f", "intent": "Square a floating-point number.", "summary": "Returns x * x.",
         "tags": ["arithmetic", "float"], "type_ast": fn([FLOAT], FLOAT), "body_ast": lam(["x"], bapp("mul", x, x)),
         "examples": [{"args": [2.5], "result": 6.25}, {"args": [-1.5], "result": 2.25}],
         "properties": [], "prove": False},
        {"name": "double_f", "intent": "Double a floating-point number.", "summary": "Returns x + x.",
         "tags": ["arithmetic", "float", "linear"], "type_ast": fn([FLOAT], FLOAT), "body_ast": lam(["x"], bapp("add", x, x)),
         "examples": [{"args": [1.5], "result": 3.0}, {"args": [-2.25], "result": -4.5}],
         "properties": [], "prove": False},
        {"name": "negate_f", "intent": "Negate a floating-point number.", "summary": "Returns -x.",
         "tags": ["arithmetic", "float"], "type_ast": fn([FLOAT], FLOAT), "body_ast": lam(["x"], bapp("neg", x)),
         "examples": [{"args": [2.5], "result": -2.5}, {"args": [-1.5], "result": 1.5}],
         "properties": [], "prove": False},
        {"name": "cube_f", "intent": "Cube a floating-point number.", "summary": "Returns x * x * x.",
         "tags": ["arithmetic", "float"], "type_ast": fn([FLOAT], FLOAT),
         "body_ast": lam(["x"], bapp("mul", x, bapp("mul", x, x))),
         "examples": [{"args": [2.0], "result": 8.0}, {"args": [-1.5], "result": -3.375}],
         "properties": [], "prove": False},
    ]


def maybe_funcs():
    # Sum-typed (Maybe) functions: total functions that RETURN an optional, constructing the variant with
    # a computed payload (`Just(a / b)`). Sum types are opaque to the prover, so these verify by
    # validate + typecheck + run (no proofs) — the new ground they cover is variant construction.
    a, b, xs = var("a"), var("b"), var("xs")
    return [
        {"name": "safe_div", "intent": "Divide two integers, returning nothing on division by zero.",
         "summary": "Just(a / b) when b is nonzero; None when b is zero.", "tags": ["arithmetic", "maybe", "partial"],
         "type_ast": fn([INT, INT], maybe_t(INT)),
         "body_ast": lam(["a", "b"], case_bool(bapp("eq", b, int_lit(0)),
                                               variant_expr("None"), variant_expr("Just", bapp("div", a, b)))),
         "examples": [{"args": [6, 2], "result": V("Just", 3)}, {"args": [7, 0], "result": V("None")},
                      {"args": [9, 3], "result": V("Just", 3)}],
         "properties": [], "prove": False},
        {"name": "safe_mod", "intent": "Modulo of two integers, returning nothing on a zero divisor.",
         "summary": "Just(a mod b) when b is nonzero; None when b is zero.", "tags": ["arithmetic", "maybe", "partial"],
         "type_ast": fn([INT, INT], maybe_t(INT)),
         "body_ast": lam(["a", "b"], case_bool(bapp("eq", b, int_lit(0)),
                                               variant_expr("None"), variant_expr("Just", bapp("mod", a, b)))),
         "examples": [{"args": [7, 3], "result": V("Just", 1)}, {"args": [5, 0], "result": V("None")},
                      {"args": [8, 4], "result": V("Just", 0)}],
         "properties": [], "prove": False},
        {"name": "first", "intent": "The first element of a list, if it has one.",
         "summary": "Just(head xs) for a non-empty list; None for the empty list.", "tags": ["list", "maybe", "safe"],
         "type_ast": poly_list_fn(maybe_t(var("a"))),
         "body_ast": lam(["xs"], case_bool(bapp("null", xs),
                                           variant_expr("None"), variant_expr("Just", bapp("head", xs)))),
         "examples": [{"args": [[]], "result": V("None")}, {"args": [[1, 2, 3]], "result": V("Just", 1)},
                      {"args": [[9]], "result": V("Just", 9)}],
         "properties": [], "prove": False},
    ]


def result_funcs():
    # Sum-typed (Result) functions: success carries a value, failure carries an error payload.
    a, b = var("a"), var("b")
    return [
        {"name": "checked_div", "intent": "Divide two integers, reporting the divisor as an error when it is zero.",
         "summary": "Ok(a / b) when b is nonzero; Err(b) when b is zero.", "tags": ["arithmetic", "result", "partial"],
         "type_ast": fn([INT, INT], result_t(INT, INT)),
         "body_ast": lam(["a", "b"], case_bool(bapp("eq", b, int_lit(0)),
                                               variant_expr("Err", b), variant_expr("Ok", bapp("div", a, b)))),
         "examples": [{"args": [8, 2], "result": V("Ok", 4)}, {"args": [5, 0], "result": V("Err", 0)},
                      {"args": [10, 5], "result": V("Ok", 2)}],
         "properties": [], "prove": False},
        {"name": "checked_sub", "intent": "Subtract, erroring when the result would go negative.",
         "summary": "Ok(a - b) when a >= b; Err(b) otherwise.", "tags": ["arithmetic", "result"],
         "type_ast": fn([INT, INT], result_t(INT, INT)),
         "body_ast": lam(["a", "b"], case_bool(bapp("ge", a, b),
                                               variant_expr("Ok", bapp("sub", a, b)), variant_expr("Err", b))),
         "examples": [{"args": [5, 3], "result": V("Ok", 2)}, {"args": [2, 6], "result": V("Err", 6)},
                      {"args": [4, 4], "result": V("Ok", 0)}],
         "properties": [], "prove": False},
    ]


def recursive_funcs():
    # RAW self-recursion in the function under test — ground the rest of the corpus doesn't cover (the
    # list families use builtins/folds, never recursion in the body being described). `self` is now bound
    # in BOTH the typechecker and the evaluator (a record body type-checks against its own signature, and
    # runs via a recursive closure), so these pass the full positive gate. The `length_rec` law is stated
    # over `self`, so the proof encodes the supplied recursive body as its own `define-fun-rec` and inducts
    # on it — the user-defined-recursion path, not a builtin.
    n, xs, ys = var("n"), var("xs"), var("ys")
    return [
        {"name": "length_rec", "intent": "Count the elements of a list, by recursion.",
         "summary": "0 for the empty list; otherwise 1 + the length of the tail.", "tags": ["list", "recursion", "measure"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], case_null("xs", int_lit(0), bapp("add", int_lit(1), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 3}, {"args": [[9]], "result": 1}],
         # Distributes over append — proved by structural induction over the supplied recursive body.
         "properties": [{"name": "length_append_self",
                         "expr": forall(["xs", "ys"], op("eq", self_app(op("append", xs, ys)),
                                                       op("add", self_app(xs), self_app(ys))))}],
         "prove": True, "terminates": "always"},
        {"name": "sum_rec", "intent": "Sum a list of numbers, by recursion.",
         "summary": "0 for the empty list; otherwise the head plus the sum of the tail.", "tags": ["list", "recursion", "arithmetic"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], case_null("xs", int_lit(0), bapp("add", bapp("head", xs), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 6}, {"args": [[5, -2, 4]], "result": 7}],
         # Sum is additive over append — proved by induction over the recursive body.
         "properties": [{"name": "sum_append",
                         "expr": forall(["xs", "ys"], op("eq", self_app(op("append", xs, ys)),
                                                       op("add", self_app(xs), self_app(ys))))}],
         "prove": True, "terminates": "always"},
        {"name": "product_rec", "intent": "Multiply a list of numbers, by recursion.",
         "summary": "1 for the empty list; otherwise the head times the product of the tail.", "tags": ["list", "recursion", "arithmetic"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], case_null("xs", int_lit(1), bapp("mul", bapp("head", xs), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": 1}, {"args": [[2, 3, 4]], "result": 24}, {"args": [[5, -2]], "result": -10}],
         # Product is multiplicative over append — proved by induction over the recursive body.
         "properties": [{"name": "product_append",
                         "expr": forall(["xs", "ys"], op("eq", self_app(op("append", xs, ys)),
                                                       op("mul", self_app(xs), self_app(ys))))}],
         "prove": True, "terminates": "always"},
        {"name": "factorial", "intent": "The factorial of a non-negative integer.",
         "summary": "1 when n is 0; otherwise n * factorial(n - 1).", "tags": ["arithmetic", "recursion"],
         "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(1),
                                          bapp("mul", n, bself(bapp("sub", n, int_lit(1)))))),
         "examples": [{"args": [0], "result": 1}, {"args": [1], "result": 1}, {"args": [3], "result": 6}, {"args": [5], "result": 120}],
         # Terminates only for n >= 0 (recurses forever on a negative), so it isn't certified `always`.
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "triangular", "intent": "The nth triangular number.",
         "summary": "0 when n is 0; otherwise n + the (n-1)th triangular number.", "tags": ["arithmetic", "recursion"],
         "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                                          bapp("add", n, bself(bapp("sub", n, int_lit(1)))))),
         "examples": [{"args": [0], "result": 0}, {"args": [1], "result": 1}, {"args": [3], "result": 6}, {"args": [5], "result": 15}],
         # As with factorial, terminates only for n >= 0.
         "properties": [], "prove": False, "terminates": "unknown"},
    ]


def recursive_list_funcs():
    # Self-recursive functions that BUILD a list (cons-recursion). `nil` is the empty-list constant (var
    # `nil`); each step conses onto the recursive call on the tail. These validate, type-check, run, AND
    # prove their length laws by induction over the supplied recursive body: the inductive prover now
    # handles a `self` that returns a LIST composed under a builtin like `length` (`double_all_rec` /
    # `increment_all_rec` are length-preserving), and a two-list-parameter `self` recursing on the first
    # with the second a spectator (`append_rec` is length-additive). `countdown_rec` (int -> list) runs
    # without a stated law.
    xs, ys, n = var("xs"), var("ys"), var("n")
    nil = var("nil")
    return [
        {"name": "double_all_rec", "intent": "Double every number in a list, by recursion.",
         "summary": "nil for the empty list; otherwise (2*head) consed onto doubling the tail.",
         "tags": ["list", "recursion", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                                           bapp("cons", bapp("mul", int_lit(2), bapp("head", xs)), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [2, -4, 6]}],
         # Preserves length — proved by induction over the list-returning recursive body.
         "properties": [{"name": "length_preserving",
                         "expr": forall(["xs"], op("eq", op("length", self_app(xs)), op("length", xs)))}],
         "prove": True, "terminates": "always"},
        {"name": "increment_all_rec", "intent": "Add one to every number in a list, by recursion.",
         "summary": "nil for the empty list; otherwise (head+1) consed onto incrementing the tail.",
         "tags": ["list", "recursion", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                                           bapp("cons", bapp("add", bapp("head", xs), int_lit(1)), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": []}, {"args": [[0, 9, -5]], "result": [1, 10, -4]}],
         "properties": [{"name": "length_preserving",
                         "expr": forall(["xs"], op("eq", op("length", self_app(xs)), op("length", xs)))}],
         "prove": True, "terminates": "always"},
        {"name": "negate_all_rec", "intent": "Negate every number in a list, by recursion.",
         "summary": "nil for the empty list; otherwise (-head) consed onto negating the tail.",
         "tags": ["list", "recursion", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                                           bapp("cons", bapp("neg", bapp("head", xs)), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [-1, 2, -3]}],
         "properties": [{"name": "length_preserving",
                         "expr": forall(["xs"], op("eq", op("length", self_app(xs)), op("length", xs)))}],
         "prove": True, "terminates": "always"},
        {"name": "square_all_rec", "intent": "Square every number in a list, by recursion.",
         "summary": "nil for the empty list; otherwise (head*head) consed onto squaring the tail.",
         "tags": ["list", "recursion", "map", "elementwise"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                                           bapp("cons", bapp("mul", bapp("head", xs), bapp("head", xs)), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [1, 4, 9]}],
         "properties": [{"name": "length_preserving",
                         "expr": forall(["xs"], op("eq", op("length", self_app(xs)), op("length", xs)))}],
         "prove": True, "terminates": "always"},
        {"name": "append_rec", "intent": "Concatenate two lists, by recursion on the first.",
         "summary": "the second list when the first is empty; otherwise head consed onto appending the tail.",
         "tags": ["list", "recursion", "lossless"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([list_of(var("a")), list_of(var("a"))], list_of(var("a")))},
         "body_ast": lam(["xs", "ys"], case_null("xs", ys,
                                                 bapp("cons", bapp("head", xs), bself(bapp("tail", xs), ys)))),
         "examples": [{"args": [[], [1]], "result": [1]}, {"args": [[1, 2], [3, 4]], "result": [1, 2, 3, 4]}],
         # Length is additive over the two arguments — proved by induction on the first list (the second
         # is a spectator carried through the recursion).
         "properties": [{"name": "length_additive",
                         "expr": forall(["xs", "ys"], op("eq", op("length", self_app(xs, ys)),
                                                       op("add", op("length", xs), op("length", ys))))}],
         "prove": True, "terminates": "always"},
        {"name": "countdown_rec", "intent": "Build the list n, n-1, …, 1 for a non-negative integer.",
         "summary": "nil when n is 0; otherwise n consed onto the countdown from n-1.",
         "tags": ["list", "recursion", "generative"], "type_ast": fn([INT], list_of(INT)),
         "body_ast": lam(["n"], case_bool(bapp("eq", n, int_lit(0)), nil,
                                          bapp("cons", n, bself(bapp("sub", n, int_lit(1)))))),
         "examples": [{"args": [0], "result": []}, {"args": [3], "result": [3, 2, 1]}, {"args": [1], "result": [1]}],
         # Terminates only for n >= 0 (recurses forever on a negative).
         "properties": [], "prove": False, "terminates": "unknown"},
    ]


def arith_laws():
    # Ternary functions whose properties are the classic algebraic laws — associativity, distributivity,
    # an identity — each PROVED over the unbounded Int/Bool domain. The law is stated as a DIFFERENT
    # expression than the body (e.g. body is `(a+b)+c`, law says it equals `a+(b+c)`), so the proof is
    # non-trivial. These exercise three-argument signatures and the algebraic core a model must internalize.
    a, b, c, n = var("a"), var("b"), var("c"), var("n")
    return [
        {"name": "sum3", "intent": "Add three numbers.", "summary": "Returns (a + b) + c.",
         "tags": ["arithmetic", "associative"], "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["a", "b", "c"], bapp("add", bapp("add", a, b), c)),
         "examples": [{"args": [1, 2, 3], "result": 6}, {"args": [0, 0, 0], "result": 0}, {"args": [-1, 2, -3], "result": -2}],
         "properties": [{"name": "associative",
                         "expr": forall(["a", "b", "c"], op("eq", self_app(a, b, c), op("add", a, op("add", b, c))))}],
         "prove": True},
        {"name": "mul_sum", "intent": "Multiply a number by the sum of two others.", "summary": "Returns a * (b + c).",
         "tags": ["arithmetic", "distributive"], "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["a", "b", "c"], bapp("mul", a, bapp("add", b, c))),
         "examples": [{"args": [2, 3, 4], "result": 14}, {"args": [0, 5, 6], "result": 0}, {"args": [-2, 1, 1], "result": -4}],
         "properties": [{"name": "distributes_over_add",
                         "expr": forall(["a", "b", "c"], op("eq", self_app(a, b, c),
                                                       op("add", op("mul", a, b), op("mul", a, c))))}],
         "prove": True},
        {"name": "add_zero", "intent": "Add zero to a number.", "summary": "Returns n + 0.",
         "tags": ["arithmetic", "identity"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("add", n, int_lit(0))),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": 7}, {"args": [-4], "result": -4}],
         "properties": [{"name": "right_identity", "expr": forall(["n"], op("eq", self_app(n), n))}],
         "prove": True},
        {"name": "mul_sub", "intent": "Multiply a number by the difference of two others.",
         "summary": "Returns a * (b - c).", "tags": ["arithmetic", "distributive"],
         "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["a", "b", "c"], bapp("mul", a, bapp("sub", b, c))),
         "examples": [{"args": [2, 5, 3], "result": 4}, {"args": [0, 7, 1], "result": 0}, {"args": [-2, 1, 4], "result": 6}],
         "properties": [{"name": "distributes_over_sub",
                         "expr": forall(["a", "b", "c"], op("eq", self_app(a, b, c),
                                                       op("sub", op("mul", a, b), op("mul", a, c))))}],
         "prove": True},
        {"name": "sub_self", "intent": "Subtract a number from itself.", "summary": "Returns n - n.",
         "tags": ["arithmetic", "identity"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("sub", n, n)),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": 0}, {"args": [-4], "result": 0}],
         "properties": [{"name": "annihilates", "expr": forall(["n"], op("eq", self_app(n), int_lit(0)))}],
         "prove": True},
    ]


def bool_laws():
    # Boolean algebraic laws — associativity and De Morgan — PROVED over the boolean fragment. Stated over
    # the BUILTINS (`and`/`or`/`not`), not `self`: the prover defaults `self`'s parameters to Int, which
    # clashes with a boolean function, whereas the builtin-stated law engages the boolean solver directly.
    a, b, c = var("a"), var("b"), var("c")
    return [
        {"name": "and3", "intent": "Logical AND of three booleans.", "summary": "Returns (a and b) and c.",
         "tags": ["boolean", "associative"], "type_ast": fn([BOOL, BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b", "c"], bapp("and", bapp("and", a, b), c)),
         "examples": [{"args": [True, True, True], "result": True}, {"args": [True, False, True], "result": False},
                      {"args": [False, True, True], "result": False}],
         "properties": [{"name": "associative",
                         "expr": forall(["a", "b", "c"], op("eq", op("and", op("and", a, b), c),
                                                       op("and", a, op("and", b, c))))}],
         "prove": True},
        {"name": "or3", "intent": "Logical OR of three booleans.", "summary": "Returns (a or b) or c.",
         "tags": ["boolean", "associative"], "type_ast": fn([BOOL, BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b", "c"], bapp("or", bapp("or", a, b), c)),
         "examples": [{"args": [False, False, False], "result": False}, {"args": [True, False, False], "result": True},
                      {"args": [False, False, True], "result": True}],
         "properties": [{"name": "associative",
                         "expr": forall(["a", "b", "c"], op("eq", op("or", op("or", a, b), c),
                                                       op("or", a, op("or", b, c))))}],
         "prove": True},
        {"name": "nand", "intent": "Logical NAND of two booleans.", "summary": "Returns not (a and b).",
         "tags": ["boolean", "de-morgan"], "type_ast": fn([BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b"], bapp("not", bapp("and", a, b))),
         "examples": [{"args": [True, True], "result": False}, {"args": [True, False], "result": True},
                      {"args": [False, False], "result": True}],
         "properties": [{"name": "de_morgan",
                         "expr": forall(["a", "b"], op("eq", op("not", op("and", a, b)),
                                                       op("or", op("not", a), op("not", b))))}],
         "prove": True},
        {"name": "nor", "intent": "Logical NOR of two booleans.", "summary": "Returns not (a or b).",
         "tags": ["boolean", "de-morgan"], "type_ast": fn([BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b"], bapp("not", bapp("or", a, b))),
         "examples": [{"args": [True, True], "result": False}, {"args": [True, False], "result": False},
                      {"args": [False, False], "result": True}],
         "properties": [{"name": "de_morgan",
                         "expr": forall(["a", "b"], op("eq", op("not", op("or", a, b)),
                                                       op("and", op("not", a), op("not", b))))}],
         "prove": True},
    ]


def order_laws():
    # Algebraic laws of the order operators `max`/`min` — idempotence, commutativity, associativity —
    # PROVED over the unbounded Int domain (max/min lower to `ite` comparisons, which z3 decides).
    a, b, c = var("a"), var("b"), var("c")
    return [
        {"name": "max_self", "intent": "The maximum of a number with itself.", "summary": "Returns max(a, a).",
         "tags": ["arithmetic", "order", "idempotent"], "type_ast": fn([INT], INT),
         "body_ast": lam(["a"], bapp("max", a, a)),
         "examples": [{"args": [5], "result": 5}, {"args": [-3], "result": -3}, {"args": [0], "result": 0}],
         "properties": [{"name": "idempotent", "expr": forall(["a"], op("eq", self_app(a), a))}],
         "prove": True},
        {"name": "max3", "intent": "The maximum of three numbers.", "summary": "Returns max(max(a, b), c).",
         "tags": ["arithmetic", "order", "associative"], "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["a", "b", "c"], bapp("max", bapp("max", a, b), c)),
         "examples": [{"args": [1, 2, 3], "result": 3}, {"args": [3, 1, 2], "result": 3}, {"args": [-1, -5, -2], "result": -1}],
         "properties": [{"name": "associative",
                         "expr": forall(["a", "b", "c"], op("eq", self_app(a, b, c), op("max", a, op("max", b, c))))}],
         "prove": True},
        {"name": "min3", "intent": "The minimum of three numbers.", "summary": "Returns min(min(a, b), c).",
         "tags": ["arithmetic", "order", "associative"], "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["a", "b", "c"], bapp("min", bapp("min", a, b), c)),
         "examples": [{"args": [1, 2, 3], "result": 1}, {"args": [3, 1, 2], "result": 1}, {"args": [-1, -5, -2], "result": -5}],
         "properties": [{"name": "associative",
                         "expr": forall(["a", "b", "c"], op("eq", self_app(a, b, c), op("min", a, op("min", b, c))))}],
         "prove": True},
        {"name": "max_comm", "intent": "The maximum of two numbers, in either order.", "summary": "Returns max(a, b).",
         "tags": ["arithmetic", "order", "commutative"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("max", a, b)),
         "examples": [{"args": [5, 3], "result": 5}, {"args": [2, 9], "result": 9}, {"args": [-2, -7], "result": -2}],
         "properties": [{"name": "commutative", "expr": forall(["a", "b"], op("eq", self_app(a, b), op("max", b, a)))}],
         "prove": True},
        {"name": "min_comm", "intent": "The minimum of two numbers, in either order.", "summary": "Returns min(a, b).",
         "tags": ["arithmetic", "order", "commutative"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("min", a, b)),
         "examples": [{"args": [5, 3], "result": 3}, {"args": [2, 9], "result": 2}, {"args": [-2, -7], "result": -7}],
         "properties": [{"name": "commutative", "expr": forall(["a", "b"], op("eq", self_app(a, b), op("min", b, a)))}],
         "prove": True},
    ]


def all_specs():
    return (unary_arith() + binary_arith() + boolean_funcs() + list_funcs()
            + list_transform_funcs() + composition_funcs() + list_fold_funcs() + float_funcs()
            + maybe_funcs() + result_funcs() + recursive_funcs() + recursive_list_funcs()
            + arith_laws() + bool_laws() + order_laws())


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
    # A spec may declare its own termination class (e.g. an unbounded self-recursion that isn't certified
    # `always` here); default to `always`, with `sum`'s fold left `unknown` for back-compat.
    terminates = spec.get("terminates", "unknown" if spec["name"] == "sum" else "always")
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
        "category": "function",
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
        terminates = spec.get("terminates", "unknown" if spec["name"] == "sum" else "always")
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
            "id": "locutio_" + ident, "modality": "nova_locutio", "category": "exchange", "polarity": "positive",
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
        ("apply_square", "Ask an agent to square 5.",
         "request/apply square to 5 → the responder asserts square(5) = 25, which re-runs true.",
         "request", "square", [5], ["agent-loop", "request", "apply"]),
        ("apply_multiply", "Ask an agent to multiply 6 and 7.",
         "request/apply mul to (6, 7) → the responder asserts mul(6, 7) = 42, which re-runs true.",
         "request", "mul2", [6, 7], ["agent-loop", "request", "apply"]),
        ("propose_double", "Propose that an agent compute double of 21.",
         "propose/apply double to 21 → the responder test-runs it and commits.",
         "propose", "double", [21], ["agent-loop", "propose"]),
    ]
    first_request_hash = None
    for ident, intent, summary, kind, tname, pyargs, tags in apply_rows:
        body = {"action": "apply", "target": by_name[tname]["hash"], "args": [to_value_ast(a) for a in pyargs]}
        req = {"schema_version": "0.2.0", "kind": kind, "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
               "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000}, "body": body}
        signed = sign_message(req, SENDER_SEED)
        if first_request_hash is None:
            first_request_hash = signed.get("hash")
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

    # request/store → ack: offer a record; the responder verifies its content-address and acknowledges.
    sreq = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000},
            "body": {"action": "store", "payload": by_name["double"], "payload_kind": "function-record-v0.2"}}
    ssigned = sign_message(sreq, SENDER_SEED)
    sreply = respond_to(ssigned, commons_dir)
    s_ok = (msg_schema_valid(ssigned) and bool(sreply) and msg_schema_valid(sreply)
            and sreply.get("kind") == "ack" and sreply.get("in_reply_to") == ssigned.get("hash"))
    emit("store_double", "Offer to store the `double` function record.",
         "request/store the double record → the responder verifies its content-address and acks.",
         ["agent-loop", "request", "store"], "request", ssigned, sreply,
         "ACKED" if s_ok else (sreply.get("kind", "NO-REPLY").upper() if sreply else "NO-REPLY"), s_ok)

    # commit → assert: a received commitment to apply is fulfilled and asserted (claim re-runs true).
    creq = {"schema_version": "0.2.0", "kind": "commit", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": None,
            "body": {"commitment": {"kind": "apply", "fn": by_name["double"]["hash"], "args": [to_value_ast(21)]},
                     "conditions": [], "expires_at": None}}
    csigned = sign_message(creq, SENDER_SEED)
    creply = respond_to(csigned, commons_dir)
    c_conf = False
    if creply and creply.get("kind") == "assert":
        vp = _write_tmp(creply)
        c_conf = cli(["verify-claim", "--records", commons_dir, vp]).returncode == 0
        os.unlink(vp)
    c_ok = (msg_schema_valid(csigned) and bool(creply) and msg_schema_valid(creply)
            and creply.get("kind") == "assert" and creply.get("in_reply_to") == csigned.get("hash") and c_conf)
    emit("commit_double", "Commit to computing double of 21.",
         "commit/apply double to 21 → the responder fulfils the commitment and asserts the result, which re-runs true.",
         ["agent-loop", "commit"], "commit", csigned, creply,
         "CONFIRMED" if c_ok else (creply.get("kind", "NO-REPLY").upper() if creply else "NO-REPLY"), c_ok)

    # retract → ack: withdraw an earlier message; the loop acknowledges the retraction.
    if first_request_hash:
        rreq = {"schema_version": "0.2.0", "kind": "retract", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
                "constraints": None, "body": {"retracts": first_request_hash, "reason": "superseded by a corrected request"}}
        rsigned = sign_message(rreq, SENDER_SEED)
        rreply = respond_to(rsigned, commons_dir)
        r_ok = (msg_schema_valid(rsigned) and bool(rreply) and msg_schema_valid(rreply)
                and rreply.get("kind") == "ack" and rreply.get("in_reply_to") == rsigned.get("hash"))
        emit("retract_request", "Withdraw an earlier request.",
             "retract a prior message → the responder acknowledges the retraction.",
             ["agent-loop", "retract"], "retract", rsigned, rreply,
             "ACKED" if r_ok else (rreply.get("kind", "NO-REPLY").upper() if rreply else "NO-REPLY"), r_ok)

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
            "category": "function" if modality == "nova_lingua" else "exchange",
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

    # 3b. Refuted property — a correct subtraction body carrying a FALSE law (claims it is commutative,
    #     self(a,b) = self(b,a)). The prover refutes it with a concrete counterexample.
    a, b = var("a"), var("b")
    body3b = lam(["a", "b"], bapp("sub", a, b))
    prop3b = [{"name": "false_commutative", "expr": forall(["a", "b"], op("eq", self_app(a, b), self_app(b, a)))}]
    rec3b = build_v2_record("subtract", fn([INT, INT], INT),
                            [{"args": [to_value_ast(5), to_value_ast(3)], "result": to_value_ast(2)}],
                            body3b, properties=prop3b, terminates="always")
    _, r3b, b3b = write_rec("falsecommute", rec3b, body3b)
    pv3b = cli(["prove", r3b, "--body", b3b])
    emit("refuted_commutativity", "nova_lingua",
         "A subtraction function that wrongly claims to be commutative.",
         "The body and example are correct, but a - b = b - a is false; the prover refutes it with a counterexample.",
         ["negative", "false-property"], {"record": rec3b, "body": body3b, "properties": prop3b},
         "prove", "REFUTED", pv3b.stdout, "REFUTED" in pv3b.stdout)

    # 3c. Wrong example for a LIST function — claims reverse([1,2,3]) = [1,2,3]. Type-checks, but FAILS on
    #     execution (reverse([1,2,3]) is [3,2,1]).
    body3c = lam(["xs"], bapp("reverse", var("xs")))
    rec3c = build_v2_record("reverse", poly_list_fn(list_of(var("a"))),
                            [{"args": [to_value_ast([1, 2, 3])], "result": to_value_ast([1, 2, 3])}],
                            body3c, terminates="always")
    d3c, r3c, _ = write_rec("wronglistexample", rec3c, body3c)
    rn3c = cli(["run", "--records", d3c, r3c])
    emit("wrong_list_example", "nova_lingua",
         "A reverse function whose worked example claims reverse([1,2,3]) = [1,2,3].",
         "Well-typed, but executing it fails: reverse([1,2,3]) is [3,2,1], not [1,2,3].",
         ["negative", "wrong-example", "list"], {"record": rec3c, "body": body3c},
         "run", "EXAMPLE-FAILED", (rn3c.stdout + rn3c.stderr), rn3c.returncode != 0)

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

    # 5. Nova Locutio — a capability-gated apply WITHOUT presenting the capability. The function declares
    #    a required capability; the request lists none, so the responder rejects it as not_authorized.
    body_cap = lam(["n"], bapp("add", n, n))
    rec_cap = build_v2_record("guarded_double", fn([INT], INT), [{"args": [to_value_ast(3)], "result": to_value_ast(6)}],
                              body_cap, terminates="always")
    rec_cap["signature"]["capabilities"] = ["cap:apply/guarded"]
    rec_cap["hash"] = content_hash(rec_cap, "fn", strip=("hash",))  # re-hash: capabilities changed the record
    dcap, _, _ = write_rec("guarded", rec_cap, body_cap)
    greq = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
            "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000},  # presents NO capabilities
            "body": {"action": "apply", "target": rec_cap["hash"], "args": [to_value_ast(3)]}}
    gsigned = sign_message(greq, SENDER_SEED)
    greply = respond_to(gsigned, dcap)
    denied = (bool(greply) and greply.get("kind") == "reject"
              and greply.get("body", {}).get("code") == "not_authorized")
    emit("capability_denied", "nova_locutio",
         "A request to apply a capability-gated function without presenting the capability.",
         "The function requires cap:apply/guarded; the request lists none, so the responder rejects it as not_authorized.",
         ["negative", "capability", "agent-loop"], {"speech_act": "request", "request": gsigned, "reply": greply},
         "capability-gate", "NOT-AUTHORIZED", json.dumps(greply.get("body")) if greply else "", denied)
    return out


# --- composition examples: assembled pipelines with derived composite metadata --------------------
#
# A distinct example category (principle 4, "assemble, don't write"): a pipeline of function records and
# the composite metadata `nl-validator compose` derives from their signatures (composability stage-to-
# stage, the union of effects/capabilities, conjunction of termination, a coarse max complexity). A
# composable pipeline is a positive example; a pipeline whose stage types don't line up is a negative one
# (the composer correctly reports NOT-COMPOSABLE).

def compose_examples(commons_dir, by_name):
    def parse_compose(text):
        meta = {"composable": text.strip().startswith("COMPOSABLE")}
        for line in text.splitlines():
            s = line.strip()
            for key in ("effects", "capabilities", "terminates", "complexity"):
                if s.startswith(key):
                    meta[key] = s[len(key):].strip()
            if s.startswith("type "):
                rest = s[len("type"):].strip()
                if " -> " in rest:
                    inp, outp = rest.split(" -> ", 1)
                    try:
                        meta["input_type"], meta["output_type"] = json.loads(inp), json.loads(outp)
                    except Exception:
                        pass
            if s.startswith("NOT-COMPOSABLE"):
                meta["reason"] = s[len("NOT-COMPOSABLE"):].strip()
        return meta

    out = []
    pipelines = [
        ("reverse_then_length", "Compose: reverse a list, then take its length.",
         "A two-stage pipeline reverse;length over List a, yielding nat.", ["reverse", "length"], True),
        ("negate_then_reverse", "Compose: negate every element, then reverse the list.",
         "A two-stage pipeline negate_all;reverse over a list of ints.", ["negate_all", "reverse"], True),
        ("filter_then_length", "Compose: keep the positives, then count them.",
         "A two-stage pipeline keep_positives;length over a list of ints, yielding nat.",
         ["keep_positives", "length"], True),
        ("square_then_sum", "Compose: square every element, then sum them.",
         "A two-stage pipeline square_all;sum over a list of ints, yielding int.", ["square_all", "sum"], True),
        ("filter_square_sum", "Compose: keep the positives, square them, then sum.",
         "A three-stage pipeline keep_positives;square_all;sum — the sum of the squares of the positive elements.",
         ["keep_positives", "square_all", "sum"], True),
        ("length_then_reverse", "Compose: take a list's length, then reverse it.",
         "length yields a nat, which cannot feed reverse's List parameter — the pipeline does NOT compose.",
         ["length", "reverse"], False),
    ]
    for ident, intent, summary, names, expect in pipelines:
        recs = [by_name[n] for n in names]
        paths = [os.path.join(commons_dir, r["hash"] + ".json") for r in recs]
        p = cli(["compose"] + paths)  # NOT-COMPOSABLE is reported on stderr (non-zero exit)
        meta = parse_compose(p.stdout + "\n" + p.stderr)
        composable = meta.get("composable", False)
        views = {"pipeline": [r["hash"] for r in recs], "stages": recs, "composite": meta}
        if expect:
            out.append({
                "id": "compose_" + ident, "modality": "nova_lingua", "category": "composition", "polarity": "positive",
                "intent": intent, "summary": summary, "tags": ["composition", "pipeline", "assemble"],
                "views": views, "verification": {"composable": composable, "checked_by": "compose"},
                "_ok": composable is True,
            })
        else:
            out.append({
                "id": "neg_compose_" + ident, "modality": "nova_lingua", "category": "composition", "polarity": "negative",
                "intent": intent, "summary": summary, "tags": ["negative", "composition", "type-error"],
                "views": views, "verification": {"expected": "rejected", "check": "compose", "verdict": "NOT-COMPOSABLE",
                                                 "rejected": not composable, "reason": meta.get("reason", "")},
                "_ok": composable is False,
            })
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
        # Composition examples — assembled pipelines and their derived composite metadata (principle 4).
        for ex in compose_examples(commons_dir, by_name):
            ok = ex.pop("_ok")
            if ok or args.keep_unverified:
                examples.append(ex)
            if not ok:
                dropped.append((ex["id"], ex["verification"]))

    with open(args.out, "w", encoding="utf-8") as fh:
        for ex in examples:
            fh.write(json.dumps(ex, ensure_ascii=False) + "\n")

    by_modality, by_polarity, by_category = {}, {}, {}
    for ex in examples:
        by_modality[ex["modality"]] = by_modality.get(ex["modality"], 0) + 1
        by_polarity[ex["polarity"]] = by_polarity.get(ex["polarity"], 0) + 1
        by_category[ex["category"]] = by_category.get(ex["category"], 0) + 1
    families = {"unary_arith": len(unary_arith()), "binary_arith": len(binary_arith()),
                "boolean_funcs": len(boolean_funcs()), "list_funcs": len(list_funcs()),
                "list_transform_funcs": len(list_transform_funcs()),
                "composition_funcs": len(composition_funcs()), "list_fold_funcs": len(list_fold_funcs()),
                "float_funcs": len(float_funcs()),
                "maybe_funcs": len(maybe_funcs()), "result_funcs": len(result_funcs()),
                "recursive_funcs": len(recursive_funcs()),
                "recursive_list_funcs": len(recursive_list_funcs()),
                "arith_laws": len(arith_laws()), "bool_laws": len(bool_laws()),
                "order_laws": len(order_laws())}
    proved = sum(1 for ex in examples if ex["category"] == "function" and ex["polarity"] == "positive"
                 for p in ex["verification"]["proofs"] if p["verdict"] == "PROVED")
    confirmed = sum(1 for ex in examples if ex["modality"] == "nova_locutio" and ex["polarity"] == "positive"
                    and ex["verification"]["outcome"] == "CONFIRMED")
    rejected = sum(1 for ex in examples if ex["polarity"] == "negative")
    manifest = {
        "corpus": os.path.basename(args.out),
        "examples": len(examples),
        "by_modality": by_modality,
        "by_polarity": by_polarity,
        "by_category": by_category,
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
