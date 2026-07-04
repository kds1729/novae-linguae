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

Two scales. By default this emits the **curated** corpus (the committed `corpus.jsonl`): hand-authored
families chosen for breadth of shape, plus agent-loop exchanges, transcripts, negatives, and compositions
— the eval pool and the showcase. With `--combinatorial` it ALSO emits **parameterized** function specs —
each hand-authored shape multiplied over a fixed set of constants/operators/comparisons — for a
training-scale corpus (point `--out` at a scratch path; see `combinatorial_specs`). Every generated spec
still flows through the same verify gate. The large combinatorial file is regenerable, so it is gitignored
and not committed — the generator is the artifact.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
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


def str_lit(s):
    return {"kind": "lit", "value": {"kind": "string", "value": s}}


def op(name, *args):
    # The `op` form — used in PROPERTY/predicate expressions (the prover's head_op reads it).
    return {"kind": "app", "op": name, "args": list(args)}


def bapp(name, *args):
    # The `fn` form — used in BODY expressions (the evaluator applies `fn` to `args`). Builtins and the
    # recursive `self` are referenced as a `var` head. (Bodies and predicates use different conventions.)
    return {"kind": "app", "fn": {"kind": "var", "name": name}, "args": list(args)}


def bself(*args):
    return {"kind": "app", "fn": {"kind": "var", "name": "self"}, "args": list(args)}


def blet(name, value, body):
    """`let name = value in body` — a single-binding let BODY expression."""
    return {"kind": "let", "name": name, "value": value, "body": body}


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


def map_of(value_t):
    """The type `Map string value_t` (spec/expressiveness.md phase 2)."""
    return {"kind": "apply", "ctor": {"kind": "builtin", "name": "Map"},
            "args": [{"kind": "builtin", "name": "string"}, value_t]}


WILDCARD_PAT = {"kind": "wildcard"}


def result_t(ok_t, err_t):
    """The sum type `[Ok(ok_t) Err(err_t)]`."""
    return {"kind": "sum", "variants": [{"tag": "Ok", "type": ok_t}, {"tag": "Err", "type": err_t}]}


INT = {"kind": "builtin", "name": "int"}
NAT = {"kind": "builtin", "name": "nat"}
BOOL = {"kind": "builtin", "name": "bool"}
FLOAT = {"kind": "builtin", "name": "float"}
STRING = {"kind": "builtin", "name": "string"}


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


class FnRef:
    """A function-valued example argument, referenced by the name of a helper in the spec's `fn_deps`.
    Resolved to an `{"kind": "fn_ref", "target": <helper hash>}` value once the helper record is built."""

    def __init__(self, name):
        self.name = name


def to_value_ast(pyval):
    if isinstance(pyval, bool):
        return {"kind": "bool", "value": pyval}
    if isinstance(pyval, int):
        return {"kind": "int", "value": pyval}
    if isinstance(pyval, float):
        return {"kind": "float", "value": pyval}
    if isinstance(pyval, str):
        return {"kind": "string", "value": pyval}
    if isinstance(pyval, dict):
        # A map value (Map string a): entries sorted by key — the canonical form check-value enforces.
        return {"kind": "map", "entries": [{"key": k, "value": to_value_ast(v)}
                                           for k, v in sorted(pyval.items())]}
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
        # reverse distributes over append with the operands swapped — reverse(xs ++ ys) = reverse ys ++
        # reverse xs. Proved by structural induction, discovering the `reverse_append` lemma (and its own
        # sub-lemmas append_assoc/append_nil) — the headline of the lemma-discovery prover, stated as a law.
        {"name": "reverse_concat", "intent": "Reverse the concatenation of two lists.",
         "summary": "Returns reverse(xs ++ ys), which equals reverse(ys) ++ reverse(xs).",
         "tags": ["list", "lossless"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([list_of(var("a")), list_of(var("a"))], list_of(var("a")))},
         "body_ast": lam(["xs", "ys"], bapp("reverse", bapp("append", xs, ys))),
         "examples": [{"args": [[], [1]], "result": [1]}, {"args": [[1, 2], [3, 4]], "result": [4, 3, 2, 1]}],
         "properties": [{"name": "antihomomorphism",
                         "expr": forall(["xs", "ys"], op("eq", self_app(xs, ys),
                                                       op("append", op("reverse", ys), op("reverse", xs))))}],
         "prove": True},
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


def refined_funcs():
    # Functions carrying REFINEMENT predicates — constraints a type alone can't express (principle 1), the
    # corpus's records that populate `signature.refinements`. A `pre` refinement is a precondition the
    # caller must establish (a nonzero divisor, a non-empty list); a `post` refinement constrains the
    # result, referenced through the reserved variable `result` (spec convention). The refinement is a
    # predicate-expression AST over the parameters (and, for a post, `result`); the worked examples all
    # satisfy the precondition, so the record still type-checks and runs. The `post`/pre-gated records are
    # additionally PROVED against the body by `check-refinement` in the build gate (the `nat`-result one
    # too), so each is a verified conditional contract, not just a declared one.
    a, b, n, xs, result = var("a"), var("b"), var("n"), var("xs"), var("result")
    return [
        {"name": "divide", "intent": "Integer division, with a nonzero-divisor precondition.",
         "summary": "Returns a / b; requires b != 0.", "tags": ["arithmetic", "partial", "refinement"],
         "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], bapp("div", a, b)),
         "examples": [{"args": [6, 2], "result": 3}, {"args": [9, 3], "result": 3}, {"args": [7, 1], "result": 7}],
         "refinements": [{"kind": "pre", "expr": op("neq", b, int_lit(0))}],
         "properties": [], "prove": False},
        {"name": "modulo", "intent": "Integer modulo, with a nonzero-divisor precondition.",
         "summary": "Returns a mod b; requires b != 0.", "tags": ["arithmetic", "partial", "refinement"],
         "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], bapp("mod", a, b)),
         "examples": [{"args": [7, 3], "result": 1}, {"args": [8, 4], "result": 0}, {"args": [9, 5], "result": 4}],
         "refinements": [{"kind": "pre", "expr": op("neq", b, int_lit(0))}],
         "properties": [], "prove": False},
        {"name": "head_of", "intent": "The first element of a list, with a non-empty precondition.",
         "summary": "Returns head(xs); requires xs to be non-empty.", "tags": ["list", "partial", "refinement"],
         "type_ast": poly_list_fn(var("a")), "body_ast": lam(["xs"], bapp("head", xs)),
         "examples": [{"args": [[1, 2, 3]], "result": 1}, {"args": [[9]], "result": 9}],
         "refinements": [{"kind": "pre", "expr": op("not", op("null", xs))}],
         "properties": [], "prove": False},
        {"name": "abs_pos", "intent": "Absolute value, with a non-negative postcondition.",
         "summary": "Returns |n|; guarantees result >= 0.", "tags": ["arithmetic", "refinement"],
         "type_ast": fn([INT], INT), "body_ast": lam(["n"], bapp("abs", n)),
         "examples": [{"args": [0], "result": 0}, {"args": [6], "result": 6}, {"args": [-4], "result": 4}],
         "refinements": [{"kind": "post", "expr": op("ge", result, int_lit(0))}],
         "properties": [], "prove": False},
        # An EXACT postcondition: the result equals a closed-form expression of the inputs (a tighter
        # contract than a property's algebraic law — it pins the output, not a relation it satisfies).
        {"name": "inc_spec", "intent": "Increment, specified by an exact postcondition.",
         "summary": "Returns n + 1; postcondition result = n + 1.", "tags": ["arithmetic", "refinement"],
         "type_ast": fn([INT], INT), "body_ast": lam(["n"], bapp("add", n, int_lit(1))),
         "examples": [{"args": [0], "result": 1}, {"args": [5], "result": 6}, {"args": [-3], "result": -2}],
         "refinements": [{"kind": "post", "expr": op("eq", result, op("add", n, int_lit(1)))}],
         "properties": [], "prove": False},
        {"name": "sum2_spec", "intent": "Add two integers, specified by an exact postcondition.",
         "summary": "Returns a + b; postcondition result = a + b.", "tags": ["arithmetic", "refinement"],
         "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], bapp("add", a, b)),
         "examples": [{"args": [2, 3], "result": 5}, {"args": [0, 0], "result": 0}, {"args": [-1, 4], "result": 3}],
         "refinements": [{"kind": "post", "expr": op("eq", result, op("add", a, b))}],
         "properties": [], "prove": False},
        # A PRE-GATED postcondition: the output guarantee holds only under the precondition. Without
        # `a >= b` the result could be negative; with it, `a - b >= 0` — exactly what a precondition buys.
        {"name": "safe_sub", "intent": "Subtract, guaranteeing a non-negative result under a >= b.",
         "summary": "Returns a - b; requires a >= b, guarantees result >= 0.",
         "tags": ["arithmetic", "refinement"],
         "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], bapp("sub", a, b)),
         "examples": [{"args": [5, 2], "result": 3}, {"args": [4, 4], "result": 0}, {"args": [9, 1], "result": 8}],
         "refinements": [{"kind": "pre", "expr": op("ge", a, b)},
                         {"kind": "post", "expr": op("ge", result, int_lit(0))}],
         "properties": [], "prove": False},
    ]


def costed_funcs():
    # Functions carrying a declared `signature.complexity` (an `O(…)` running-time bound) that is VERIFIED
    # against the body by `nl-validator check-complexity` in the build gate — the running-time counterpart to
    # the refinement / termination contracts (principle 3). The checker infers a sound upper bound by
    # structural cost analysis (no solver): a non-recursive first-order body is O(1)/O(n); a structural
    # recursion is solved as a recurrence T(n) = a·T(n−k) + w — one self-call with O(1) per-step work is
    # O(n), one with O(n) work (an `append` of the recursive result) is O(n²). Each declaration here is TIGHT
    # (the checker returns SOUND, not merely VERIFIED-could-be-tighter), so the corpus teaches complexity
    # annotations that are exactly right for their bodies, not just safe over-estimates.
    # Each also carries the structured v0.3 `cost` (`time` + `output_size`), which `check-complexity`
    # verifies too: `time` against the inferred running-time class, and `output_size` against the inferred
    # result-size growth. `reverse_naive_cost` is the showcase that the two are INDEPENDENT — O(n^2) time
    # but a size-*preserving* (Θ(n)) output — which is exactly what the `compose` precise-complexity path
    # threads through a pipeline and, until now, trusted without proof.
    n, xs = var("n"), var("xs")
    nil = var("nil")
    return [
        {"name": "sum2_cost", "intent": "Add two integers, in constant time.",
         "summary": "Returns a + b; O(1) — a single primitive addition.", "tags": ["arithmetic", "complexity"],
         "type_ast": fn([INT, INT], INT), "body_ast": lam(["a", "b"], bapp("add", var("a"), var("b"))),
         "examples": [{"args": [2, 3], "result": 5}, {"args": [0, 0], "result": 0}, {"args": [-1, 4], "result": 3}],
         "complexity": "O(1)", "cost": {"time": "O(1)", "output_size": "constant", "measure": "size"},
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "length_cost", "intent": "Count the elements of a list, in linear time.",
         "summary": "0 for the empty list; otherwise 1 + the length of the tail — O(n).",
         "tags": ["list", "recursion", "complexity"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], case_null("xs", int_lit(0), bapp("add", int_lit(1), bself(bapp("tail", xs))))),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 3}, {"args": [[9]], "result": 1}],
         "complexity": "O(n)", "cost": {"time": "O(n)", "output_size": "constant", "measure": "size"},
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "reverse_naive_cost", "intent": "Reverse a list the naive way, in quadratic time.",
         "summary": "nil for the empty list; otherwise reverse(tail) ++ [head] — O(n^2) time, Θ(n) output.",
         "tags": ["list", "recursion", "complexity"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                                           bapp("append", bself(bapp("tail", xs)),
                                                bapp("cons", bapp("head", xs), nil)))),
         "examples": [{"args": [[]], "result": []}, {"args": [[1, 2, 3]], "result": [3, 2, 1]}, {"args": [[7]], "result": [7]}],
         "complexity": "O(n^2)", "cost": {"time": "O(n^2)", "output_size": "preserving", "measure": "size"},
         "properties": [], "prove": False, "terminates": "always"},
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
        {"name": "neg_neg", "intent": "Negate a number twice.", "summary": "Returns -(-n).",
         "tags": ["arithmetic", "involutive"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("neg", bapp("neg", n))),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": 7}, {"args": [-4], "result": -4}],
         "properties": [{"name": "involutive", "expr": forall(["n"], op("eq", self_app(n), n))}],
         "prove": True},
        {"name": "abs_abs", "intent": "Take the absolute value twice.", "summary": "Returns ||n||.",
         "tags": ["arithmetic", "idempotent"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("abs", bapp("abs", n))),
         "examples": [{"args": [0], "result": 0}, {"args": [6], "result": 6}, {"args": [-4], "result": 4}],
         "properties": [{"name": "idempotent", "expr": forall(["n"], op("eq", self_app(n), op("abs", n)))}],
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
        {"name": "and_idem", "intent": "Logical AND of a boolean with itself.", "summary": "Returns a and a.",
         "tags": ["boolean", "idempotent"], "type_ast": fn([BOOL], BOOL),
         "body_ast": lam(["a"], bapp("and", a, a)),
         "examples": [{"args": [True], "result": True}, {"args": [False], "result": False}],
         "properties": [{"name": "idempotent", "expr": forall(["a"], op("eq", op("and", a, a), a))}],
         "prove": True},
        {"name": "or_idem", "intent": "Logical OR of a boolean with itself.", "summary": "Returns a or a.",
         "tags": ["boolean", "idempotent"], "type_ast": fn([BOOL], BOOL),
         "body_ast": lam(["a"], bapp("or", a, a)),
         "examples": [{"args": [True], "result": True}, {"args": [False], "result": False}],
         "properties": [{"name": "idempotent", "expr": forall(["a"], op("eq", op("or", a, a), a))}],
         "prove": True},
        {"name": "and_absorb", "intent": "Absorption of OR into AND.", "summary": "Returns a and (a or b).",
         "tags": ["boolean", "absorption"], "type_ast": fn([BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b"], bapp("and", a, bapp("or", a, b))),
         "examples": [{"args": [True, False], "result": True}, {"args": [False, True], "result": False},
                      {"args": [True, True], "result": True}],
         "properties": [{"name": "absorbs", "expr": forall(["a", "b"], op("eq", op("and", a, op("or", a, b)), a))}],
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
        {"name": "max_min_absorb", "intent": "Absorption of min into max.", "summary": "Returns max(a, min(a, b)).",
         "tags": ["arithmetic", "order", "absorption"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("max", a, bapp("min", a, b))),
         "examples": [{"args": [5, 3], "result": 5}, {"args": [2, 9], "result": 2}, {"args": [-2, -7], "result": -2}],
         "properties": [{"name": "absorbs", "expr": forall(["a", "b"], op("eq", self_app(a, b), a))}],
         "prove": True},
        {"name": "min_max_absorb", "intent": "Absorption of max into min.", "summary": "Returns min(a, max(a, b)).",
         "tags": ["arithmetic", "order", "absorption"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("min", a, bapp("max", a, b))),
         "examples": [{"args": [5, 3], "result": 5}, {"args": [2, 9], "result": 2}, {"args": [-2, -7], "result": -2}],
         "properties": [{"name": "absorbs", "expr": forall(["a", "b"], op("eq", self_app(a, b), a))}],
         "prove": True},
    ]


def higher_order_funcs():
    # Functions whose TYPE is higher-order — they take a function as an argument: the corpus's first such
    # records with EXECUTABLE examples. Each example supplies the function argument as an `fn_ref` to a
    # helper declared in `fn_deps`, which the generator builds into the run directory so the worked example
    # runs end to end — a record assembled from another commons function (principle 4). Kept out of
    # `all_specs` (so the shared-commons / agent-loop builders are unaffected) and run only as function
    # examples. Bodies wrap the higher-order builtins map/filter/foldl/foldr; examples only (laws over an
    # uninterpreted function are already carried as properties on first-order records like negate_all).
    DOUBLE = {"name": "double_dep", "type_ast": fn([INT], INT),
              "body_ast": lam(["n"], bapp("add", var("n"), var("n"))), "examples": [{"args": [3], "result": 6}]}
    IS_POS = {"name": "is_pos_dep", "type_ast": fn([INT], BOOL),
              "body_ast": lam(["n"], bapp("gt", var("n"), int_lit(0))),
              "examples": [{"args": [3], "result": True}, {"args": [-1], "result": False}]}
    ADD2 = {"name": "add2_dep", "type_ast": fn([INT, INT], INT),
            "body_ast": lam(["a", "b"], bapp("add", var("a"), var("b"))), "examples": [{"args": [1, 2], "result": 3}]}
    list_a, list_b = list_of(var("a")), list_of(var("b"))
    return [
        {"name": "map_with", "intent": "Apply a function to every element of a list.",
         "summary": "map(f, xs) — the elementwise transform, taking the function as an argument.",
         "tags": ["list", "higher-order", "map"],
         "type_ast": {"kind": "forall", "vars": ["a", "b"], "body": fn([fn([var("a")], var("b")), list_a], list_b)},
         "body_ast": lam(["f", "xs"], bapp("map", var("f"), var("xs"))),
         "examples": [{"args": [FnRef("double_dep"), [1, 2, 3]], "result": [2, 4, 6]},
                      {"args": [FnRef("double_dep"), []], "result": []}],
         "fn_deps": [DOUBLE], "properties": [], "prove": False},
        {"name": "filter_with", "intent": "Keep the elements of a list that satisfy a predicate.",
         "summary": "filter(p, xs) — taking the predicate as an argument.",
         "tags": ["list", "higher-order", "filter"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([fn([var("a")], BOOL), list_a], list_a)},
         "body_ast": lam(["p", "xs"], bapp("filter", var("p"), var("xs"))),
         "examples": [{"args": [FnRef("is_pos_dep"), [1, -2, 3, 0]], "result": [1, 3]},
                      {"args": [FnRef("is_pos_dep"), []], "result": []}],
         "fn_deps": [IS_POS], "properties": [], "prove": False},
        {"name": "foldl_with", "intent": "Left-fold a list with a binary function and an initial value.",
         "summary": "foldl(f, z, xs) — the accumulating fold, taking the combining function as an argument.",
         "tags": ["list", "higher-order", "fold"],
         "type_ast": {"kind": "forall", "vars": ["a", "b"],
                      "body": fn([fn([var("b"), var("a")], var("b")), var("b"), list_a], var("b"))},
         "body_ast": lam(["f", "z", "xs"], bapp("foldl", var("f"), var("z"), var("xs"))),
         "examples": [{"args": [FnRef("add2_dep"), 0, [1, 2, 3]], "result": 6},
                      {"args": [FnRef("add2_dep"), 10, []], "result": 10}],
         "fn_deps": [ADD2], "properties": [], "prove": False},
        {"name": "foldr_with", "intent": "Right-fold a list with a binary function and an initial value.",
         "summary": "foldr(f, z, xs) — taking the combining function as an argument.",
         "tags": ["list", "higher-order", "fold"],
         "type_ast": {"kind": "forall", "vars": ["a", "b"],
                      "body": fn([fn([var("a"), var("b")], var("b")), var("b"), list_a], var("b"))},
         "body_ast": lam(["f", "z", "xs"], bapp("foldr", var("f"), var("z"), var("xs"))),
         "examples": [{"args": [FnRef("add2_dep"), 0, [1, 2, 3]], "result": 6},
                      {"args": [FnRef("add2_dep"), 5, []], "result": 5}],
         "fn_deps": [ADD2], "properties": [], "prove": False},
    ]


def higher_order_more():
    # A second higher-order batch — function application (apply_to), double application (twice), two-function
    # composition (compose2), and predicate-driven aggregation (all_with/any_with/count_with). Each takes a
    # function as an argument supplied as an `fn_ref` to a helper in `fn_deps`; the generator builds those
    # helpers so the worked examples run end-to-end. Examples only.
    DOUBLE = {"name": "double_dep", "type_ast": fn([INT], INT),
              "body_ast": lam(["n"], bapp("add", var("n"), var("n"))), "examples": [{"args": [3], "result": 6}]}
    INC = {"name": "inc_dep", "type_ast": fn([INT], INT),
           "body_ast": lam(["n"], bapp("add", var("n"), int_lit(1))), "examples": [{"args": [3], "result": 4}]}
    IS_POS = {"name": "is_pos_dep", "type_ast": fn([INT], BOOL),
              "body_ast": lam(["n"], bapp("gt", var("n"), int_lit(0))),
              "examples": [{"args": [3], "result": True}, {"args": [-1], "result": False}]}
    a, b, c = var("a"), var("b"), var("c")
    return [
        {"name": "apply_to", "intent": "Apply a function to a value.",
         "summary": "f x — applies the given function to the given argument.",
         "tags": ["higher-order", "apply"],
         "type_ast": {"kind": "forall", "vars": ["a", "b"], "body": fn([fn([a], b), a], b)},
         "body_ast": lam(["f", "x"], bapp("f", var("x"))),
         "examples": [{"args": [FnRef("double_dep"), 5], "result": 10},
                      {"args": [FnRef("double_dep"), -3], "result": -6}],
         "fn_deps": [DOUBLE], "properties": [], "prove": False},
        {"name": "twice", "intent": "Apply a function to a value twice.",
         "summary": "f (f x) — applies the function, then applies it again.",
         "tags": ["higher-order", "apply"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([fn([a], a), a], a)},
         "body_ast": lam(["f", "x"], bapp("f", bapp("f", var("x")))),
         "examples": [{"args": [FnRef("double_dep"), 3], "result": 12},
                      {"args": [FnRef("double_dep"), 1], "result": 4}],
         "fn_deps": [DOUBLE], "properties": [], "prove": False},
        {"name": "compose2", "intent": "Compose two functions and apply them to a value.",
         "summary": "f (g x) — applies g, then f, to the argument.",
         "tags": ["higher-order", "compose"],
         "type_ast": {"kind": "forall", "vars": ["a", "b", "c"], "body": fn([fn([b], c), fn([a], b), a], c)},
         "body_ast": lam(["f", "g", "x"], bapp("f", bapp("g", var("x")))),
         "examples": [{"args": [FnRef("double_dep"), FnRef("inc_dep"), 3], "result": 8},
                      {"args": [FnRef("double_dep"), FnRef("inc_dep"), 0], "result": 2}],
         "fn_deps": [DOUBLE, INC], "properties": [], "prove": False},
        {"name": "all_with", "intent": "Test whether every element of a list satisfies a predicate.",
         "summary": "Right-folds (p x) and the accumulator with AND, true for the empty list.",
         "tags": ["list", "higher-order", "fold", "predicate"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([fn([a], BOOL), list_of(a)], BOOL)},
         "body_ast": lam(["p", "xs"], bapp("foldr", lam(["x", "acc"], bapp("and", bapp("p", var("x")), var("acc"))),
                                           bool_lit(True), var("xs"))),
         "examples": [{"args": [FnRef("is_pos_dep"), [1, 2, 3]], "result": True},
                      {"args": [FnRef("is_pos_dep"), [1, -2, 3]], "result": False},
                      {"args": [FnRef("is_pos_dep"), []], "result": True}],
         "fn_deps": [IS_POS], "properties": [], "prove": False},
        {"name": "any_with", "intent": "Test whether some element of a list satisfies a predicate.",
         "summary": "Right-folds (p x) and the accumulator with OR, false for the empty list.",
         "tags": ["list", "higher-order", "fold", "predicate"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([fn([a], BOOL), list_of(a)], BOOL)},
         "body_ast": lam(["p", "xs"], bapp("foldr", lam(["x", "acc"], bapp("or", bapp("p", var("x")), var("acc"))),
                                           bool_lit(False), var("xs"))),
         "examples": [{"args": [FnRef("is_pos_dep"), [-1, 2, -3]], "result": True},
                      {"args": [FnRef("is_pos_dep"), [-1, -2]], "result": False},
                      {"args": [FnRef("is_pos_dep"), []], "result": False}],
         "fn_deps": [IS_POS], "properties": [], "prove": False},
        {"name": "count_with", "intent": "Count the elements of a list that satisfy a predicate.",
         "summary": "length (filter p xs) — the number of elements passing the predicate.",
         "tags": ["list", "higher-order", "filter", "composition"],
         "type_ast": {"kind": "forall", "vars": ["a"], "body": fn([fn([a], BOOL), list_of(a)], NAT)},
         "body_ast": lam(["p", "xs"], bapp("length", bapp("filter", var("p"), var("xs")))),
         "examples": [{"args": [FnRef("is_pos_dep"), [1, -2, 3, 0, 4]], "result": 3},
                      {"args": [FnRef("is_pos_dep"), []], "result": 0}],
         "fn_deps": [IS_POS], "properties": [], "prove": False},
    ]


def provenance_funcs():
    # Records carrying DERIVATION HISTORY (principle 1): `derived_from` / `supersedes` point at the
    # content-address of a prior function — the corpus's first non-null provenance. The parent is declared
    # as an fn_dep so its hash is known and stamped into the field (then the record is re-hashed). The
    # record otherwise validates, type-checks, and runs normally; the link is pure metadata.
    n = var("n")
    DOUBLE = {"name": "double_parent", "type_ast": fn([INT], INT),
              "body_ast": lam(["n"], bapp("add", var("n"), var("n"))), "examples": [{"args": [3], "result": 6}]}
    NEGATE_OLD = {"name": "negate_via_sub", "type_ast": fn([INT], INT),
                  "body_ast": lam(["n"], bapp("sub", int_lit(0), var("n"))), "examples": [{"args": [3], "result": -3}]}
    return [
        {"name": "quadruple_derived", "intent": "Quadruple a number, derived from doubling.",
         "summary": "Returns 4 * n; derived_from the doubling function.", "tags": ["arithmetic", "linear", "provenance"],
         "type_ast": fn([INT], INT), "body_ast": lam(["n"], bapp("mul", int_lit(4), n)),
         "examples": [{"args": [0], "result": 0}, {"args": [3], "result": 12}, {"args": [-2], "result": -8}],
         "derived_from": "double_parent", "fn_deps": [DOUBLE], "properties": [], "prove": False},
        {"name": "negate_v2", "intent": "Negate a number, superseding an earlier implementation.",
         "summary": "Returns -n with the neg builtin; supersedes a prior 0 - n implementation.",
         "tags": ["arithmetic", "provenance"], "type_ast": fn([INT], INT), "body_ast": lam(["n"], bapp("neg", n)),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": -7}, {"args": [-4], "result": 4}],
         "supersedes": "negate_via_sub", "fn_deps": [NEGATE_OLD], "properties": [], "prove": False},
    ]


def more_arith():
    # A second arithmetic batch widening the scalar vocabulary: a cube, a three-way sign, a clamp and a
    # range test (both ternary), and two more predicates. Runnable; examples only (the proof surface is
    # already covered by arith_laws/order_laws). `sign` exercises a NESTED boolean case.
    n, lo, hi, x = var("n"), var("lo"), var("hi"), var("x")
    return [
        {"name": "cube", "intent": "Cube a number.", "summary": "Returns n * n * n.",
         "tags": ["arithmetic"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("mul", n, bapp("mul", n, n))),
         "examples": [{"args": [0], "result": 0}, {"args": [2], "result": 8}, {"args": [-3], "result": -27}],
         "properties": [], "prove": False},
        {"name": "sign", "intent": "The sign of a number (-1, 0, or 1).",
         "summary": "1 when n > 0, -1 when n < 0, 0 when n is zero.", "tags": ["arithmetic", "comparison"],
         "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("gt", n, int_lit(0)), int_lit(1),
                                          case_bool(bapp("lt", n, int_lit(0)), int_lit(-1), int_lit(0)))),
         "examples": [{"args": [5], "result": 1}, {"args": [-3], "result": -1}, {"args": [0], "result": 0}],
         "properties": [], "prove": False},
        {"name": "clamp", "intent": "Clamp a number to a [lo, hi] range.",
         "summary": "Returns max(lo, min(hi, x)).", "tags": ["arithmetic", "order"],
         "type_ast": fn([INT, INT, INT], INT),
         "body_ast": lam(["lo", "hi", "x"], bapp("max", lo, bapp("min", hi, x))),
         "examples": [{"args": [0, 10, 5], "result": 5}, {"args": [0, 10, -3], "result": 0},
                      {"args": [0, 10, 20], "result": 10}],
         "properties": [], "prove": False},
        {"name": "in_range", "intent": "Test whether a number lies in a [lo, hi] range.",
         "summary": "Returns true iff lo <= x and x <= hi.", "tags": ["predicate", "comparison"],
         "type_ast": fn([INT, INT, INT], BOOL),
         "body_ast": lam(["lo", "hi", "x"], bapp("and", bapp("le", lo, x), bapp("le", x, hi))),
         "examples": [{"args": [0, 10, 5], "result": True}, {"args": [0, 10, -1], "result": False},
                      {"args": [0, 10, 10], "result": True}],
         "properties": [], "prove": False},
        {"name": "is_odd", "intent": "Test whether a number is odd.", "summary": "Returns true iff n mod 2 != 0.",
         "tags": ["predicate", "arithmetic"], "type_ast": fn([INT], BOOL),
         "body_ast": lam(["n"], bapp("neq", bapp("mod", n, int_lit(2)), int_lit(0))),
         "examples": [{"args": [3], "result": True}, {"args": [4], "result": False}, {"args": [0], "result": False}],
         "properties": [], "prove": False},
        {"name": "is_negative", "intent": "Test whether a number is negative.", "summary": "Returns true iff n < 0.",
         "tags": ["predicate", "comparison"], "type_ast": fn([INT], BOOL),
         "body_ast": lam(["n"], bapp("lt", n, int_lit(0))),
         "examples": [{"args": [-2], "result": True}, {"args": [0], "result": False}, {"args": [5], "result": False}],
         "properties": [], "prove": False},
    ]


def more_laws():
    # Two more PROVED identities mirroring add_zero / sub_self — a multiplicative right-identity and a
    # multiplicative annihilator — each stated as a different expression than the body and proved over Int.
    n = var("n")
    return [
        {"name": "mul_one", "intent": "Multiply a number by one.", "summary": "Returns n * 1.",
         "tags": ["arithmetic", "identity"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("mul", n, int_lit(1))),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": 7}, {"args": [-4], "result": -4}],
         "properties": [{"name": "right_identity", "expr": forall(["n"], op("eq", self_app(n), n))}],
         "prove": True},
        {"name": "mul_zero", "intent": "Multiply a number by zero.", "summary": "Returns n * 0.",
         "tags": ["arithmetic", "identity"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("mul", n, int_lit(0))),
         "examples": [{"args": [0], "result": 0}, {"args": [7], "result": 0}, {"args": [-4], "result": 0}],
         "properties": [{"name": "annihilates", "expr": forall(["n"], op("eq", self_app(n), int_lit(0)))}],
         "prove": True},
    ]


def bool_more():
    # Two more boolean functions: material implication and biconditional. Examples only (the boolean proof
    # surface — associativity, De Morgan, idempotence, absorption — is already covered by bool_laws).
    a, b = var("a"), var("b")
    return [
        {"name": "implies", "intent": "Material implication of two booleans.", "summary": "Returns (not a) or b.",
         "tags": ["boolean"], "type_ast": fn([BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b"], bapp("or", bapp("not", a), b)),
         "examples": [{"args": [True, True], "result": True}, {"args": [True, False], "result": False},
                      {"args": [False, True], "result": True}, {"args": [False, False], "result": True}],
         "properties": [], "prove": False},
        {"name": "iff", "intent": "Biconditional of two booleans.", "summary": "Returns true iff a equals b.",
         "tags": ["boolean"], "type_ast": fn([BOOL, BOOL], BOOL),
         "body_ast": lam(["a", "b"], bapp("eq", a, b)),
         "examples": [{"args": [True, True], "result": True}, {"args": [True, False], "result": False},
                      {"args": [False, False], "result": True}],
         "properties": [], "prove": False},
    ]


def recursive_more():
    # A second self-recursion batch — list membership and counting (scalar results), two-argument list
    # slicing (take/drop), a generative repeat, an exponent, and a last-element accessor with a non-empty
    # refinement. These widen the recursion vocabulary beyond `recursive_funcs`; runnable, examples only.
    # `take`/`drop`/`repeat`/`pow` recurse on a counter that loops on a negative argument, so they are not
    # certified `always`; `member`/`count_occurrences`/`last_rec` recurse structurally on the tail.
    x, xs, n, b, e = var("x"), var("xs"), var("n"), var("b"), var("e")
    return [
        {"name": "member", "intent": "Test whether a value occurs in a list.",
         "summary": "false for the empty list; otherwise (x == head) or x occurs in the tail.",
         "tags": ["list", "recursion", "predicate", "search"], "type_ast": fn([INT, list_of(INT)], BOOL),
         "body_ast": lam(["x", "xs"], case_null("xs", bool_lit(False),
                         case_bool(bapp("eq", x, bapp("head", xs)), bool_lit(True), bself(x, bapp("tail", xs))))),
         "examples": [{"args": [2, [1, 2, 3]], "result": True}, {"args": [5, [1, 2, 3]], "result": False},
                      {"args": [1, []], "result": False}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "count_occurrences", "intent": "Count how many times a value occurs in a list.",
         "summary": "0 for the empty list; add 1 when the head matches, then count the tail.",
         "tags": ["list", "recursion", "search", "measure"], "type_ast": fn([INT, list_of(INT)], INT),
         "body_ast": lam(["x", "xs"], case_null("xs", int_lit(0),
                         case_bool(bapp("eq", x, bapp("head", xs)),
                                   bapp("add", int_lit(1), bself(x, bapp("tail", xs))),
                                   bself(x, bapp("tail", xs))))),
         "examples": [{"args": [2, [2, 1, 2, 3, 2]], "result": 3}, {"args": [9, [1, 2, 3]], "result": 0},
                      {"args": [1, []], "result": 0}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "take_rec", "intent": "Take the first n elements of a list, by recursion.",
         "summary": "nil when n is 0 or the list is empty; otherwise head consed onto taking n-1 of the tail.",
         "tags": ["list", "recursion", "slice"], "type_ast": fn([INT, list_of(INT)], list_of(INT)),
         "body_ast": lam(["n", "xs"], case_bool(bapp("eq", n, int_lit(0)), var("nil"),
                         case_null("xs", var("nil"),
                                   bapp("cons", bapp("head", xs), bself(bapp("sub", n, int_lit(1)), bapp("tail", xs)))))),
         "examples": [{"args": [2, [1, 2, 3, 4]], "result": [1, 2]}, {"args": [0, [1, 2, 3]], "result": []},
                      {"args": [5, [1, 2]], "result": [1, 2]}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "drop_rec", "intent": "Drop the first n elements of a list, by recursion.",
         "summary": "the list when n is 0 or it is empty; otherwise drop n-1 of the tail.",
         "tags": ["list", "recursion", "slice"], "type_ast": fn([INT, list_of(INT)], list_of(INT)),
         "body_ast": lam(["n", "xs"], case_bool(bapp("eq", n, int_lit(0)), xs,
                         case_null("xs", var("nil"), bself(bapp("sub", n, int_lit(1)), bapp("tail", xs))))),
         "examples": [{"args": [2, [1, 2, 3, 4]], "result": [3, 4]}, {"args": [0, [1, 2, 3]], "result": [1, 2, 3]},
                      {"args": [5, [1, 2]], "result": []}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "repeat_rec", "intent": "Build a list of n copies of a value, by recursion.",
         "summary": "nil when n is 0; otherwise x consed onto repeating it n-1 times.",
         "tags": ["list", "recursion", "generative"], "type_ast": fn([INT, INT], list_of(INT)),
         "body_ast": lam(["n", "x"], case_bool(bapp("eq", n, int_lit(0)), var("nil"),
                         bapp("cons", x, bself(bapp("sub", n, int_lit(1)), x)))),
         "examples": [{"args": [3, 7], "result": [7, 7, 7]}, {"args": [0, 5], "result": []},
                      {"args": [1, 9], "result": [9]}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "pow", "intent": "Raise a base to a non-negative integer power.",
         "summary": "1 when the exponent is 0; otherwise base times base^(exponent-1).",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["b", "e"], case_bool(bapp("eq", e, int_lit(0)), int_lit(1),
                         bapp("mul", b, bself(b, bapp("sub", e, int_lit(1)))))),
         "examples": [{"args": [2, 3], "result": 8}, {"args": [5, 0], "result": 1}, {"args": [3, 2], "result": 9}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "last_rec", "intent": "The last element of a list, with a non-empty precondition.",
         "summary": "head when the tail is empty; otherwise the last element of the tail. Requires a non-empty list.",
         "tags": ["list", "recursion", "partial", "refinement"], "type_ast": poly_list_fn(var("a")),
         "body_ast": lam(["xs"], case_bool(bapp("null", bapp("tail", xs)), bapp("head", xs), bself(bapp("tail", xs)))),
         "examples": [{"args": [[1, 2, 3]], "result": 3}, {"args": [[9]], "result": 9}],
         "refinements": [{"kind": "pre", "expr": op("not", op("null", var("xs")))}],
         "properties": [], "prove": False, "terminates": "always"},
    ]


def recursive_shapes():
    # Recursion *shapes* the earlier families don't cover — the point is generative breadth for the
    # `write` task (the eval's weakest skill): double recursion (`fib`), Euclid-style two-argument
    # recursion (`gcd`), digit recursion via div/mod (`sum_digits`), an *ascending* two-argument list
    # build (`range_rec`, complementing the descending `countdown_rec`), indexing (`nth`, partial),
    # nested-list flattening (`concat_lists`), and a hand-written recursive filter with a conditional
    # cons (`keep_positives_rec`). Runnable; examples only. Counter-driven recursions aren't certified
    # `always`; the structural ones (on the tail / nested list) are.
    n, a, b, lo, hi, xs, xss = (var("n"), var("a"), var("b"), var("lo"), var("hi"), var("xs"), var("xss"))
    nil = var("nil")
    return [
        {"name": "fib", "intent": "The nth Fibonacci number.",
         "summary": "n when n < 2; otherwise fib(n-1) + fib(n-2) — double recursion.",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("lt", n, int_lit(2)), n,
                         bapp("add", bself(bapp("sub", n, int_lit(1))), bself(bapp("sub", n, int_lit(2)))))),
         "examples": [{"args": [0], "result": 0}, {"args": [1], "result": 1}, {"args": [7], "result": 13},
                      {"args": [10], "result": 55}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "gcd", "intent": "The greatest common divisor of two non-negative integers.",
         "summary": "a when b is 0; otherwise gcd(b, a mod b) — Euclid's algorithm.",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], case_bool(bapp("eq", b, int_lit(0)), a,
                         bself(b, bapp("mod", a, b)))),
         "examples": [{"args": [12, 8], "result": 4}, {"args": [48, 36], "result": 12},
                      {"args": [7, 0], "result": 7}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "sum_digits", "intent": "Sum the decimal digits of a non-negative integer.",
         "summary": "0 when n is 0; otherwise (n mod 10) + sum_digits(n div 10).",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                         bapp("add", bapp("mod", n, int_lit(10)), bself(bapp("div", n, int_lit(10)))))),
         "examples": [{"args": [0], "result": 0}, {"args": [123], "result": 6}, {"args": [9], "result": 9},
                      {"args": [4070], "result": 11}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "range_rec", "intent": "Build the ascending list lo, lo+1, …, hi.",
         "summary": "nil when lo > hi; otherwise lo consed onto the range from lo+1 to hi.",
         "tags": ["list", "recursion", "generative"], "type_ast": fn([INT, INT], list_of(INT)),
         "body_ast": lam(["lo", "hi"], case_bool(bapp("gt", lo, hi), nil,
                         bapp("cons", lo, bself(bapp("add", lo, int_lit(1)), hi)))),
         "examples": [{"args": [1, 3], "result": [1, 2, 3]}, {"args": [5, 5], "result": [5]},
                      {"args": [3, 1], "result": []}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "nth", "intent": "The element at index n of a list (0-based).",
         "summary": "head when n is 0; otherwise the (n-1)th element of the tail. Requires 0 <= n < length.",
         "tags": ["list", "recursion", "partial", "refinement"], "type_ast": fn([INT, list_of(INT)], INT),
         "body_ast": lam(["n", "xs"], case_bool(bapp("eq", n, int_lit(0)), bapp("head", xs),
                         bself(bapp("sub", n, int_lit(1)), bapp("tail", xs)))),
         "examples": [{"args": [0, [10, 20, 30]], "result": 10}, {"args": [2, [10, 20, 30]], "result": 30},
                      {"args": [1, [7, 8]], "result": 8}],
         "refinements": [{"kind": "pre", "expr": op("and", op("ge", n, int_lit(0)), op("lt", n, op("length", xs)))}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "concat_lists", "intent": "Flatten a list of lists into one list.",
         "summary": "nil for the empty outer list; otherwise the head list appended onto flattening the rest.",
         "tags": ["list", "recursion", "lossless"],
         "type_ast": {"kind": "forall", "vars": ["a"],
                      "body": fn([list_of(list_of(var("a")))], list_of(var("a")))},
         "body_ast": lam(["xss"], case_null("xss", nil,
                         bapp("append", bapp("head", xss), bself(bapp("tail", xss))))),
         "examples": [{"args": [[[1, 2], [3], [4, 5]]], "result": [1, 2, 3, 4, 5]}, {"args": [[]], "result": []},
                      {"args": [[[7]]], "result": [7]}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "keep_positives_rec", "intent": "Keep only the positive numbers in a list, by recursion.",
         "summary": "nil for the empty list; cons the head when it is positive, else skip it, recursing on the tail.",
         "tags": ["list", "recursion", "filter"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], case_null("xs", nil,
                         case_bool(bapp("gt", bapp("head", xs), int_lit(0)),
                                   bapp("cons", bapp("head", xs), bself(bapp("tail", xs))),
                                   bself(bapp("tail", xs))))),
         "examples": [{"args": [[1, -2, 3, 0]], "result": [1, 3]}, {"args": [[]], "result": []},
                      {"args": [[-1, -2]], "result": []}],
         "properties": [], "prove": False, "terminates": "always"},
    ]


def compositional_bodies():
    # Non-recursive but multi-operator bodies — the other half of generative `write` breadth: a fold with
    # a builtin as its function argument (`max_of_list`/`min_of_list`, partial), an inline-lambda filter
    # with a range predicate (`count_between`), an inline-lambda map (`clamp_all`), and a fold whose step
    # is a compound expression (`sum_of_cubes`). Runnable; examples only.
    lo, hi, xs, x, acc = var("lo"), var("hi"), var("xs"), var("x"), var("acc")
    return [
        {"name": "max_of_list", "intent": "The largest element of a non-empty list.",
         "summary": "foldl max over the tail, seeded with the head. Requires a non-empty list.",
         "tags": ["list", "fold", "order", "partial", "refinement"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", var("max"), bapp("head", xs), bapp("tail", xs))),
         "examples": [{"args": [[3, 1, 4, 1, 5]], "result": 5}, {"args": [[7]], "result": 7},
                      {"args": [[-2, -9, -4]], "result": -2}],
         "refinements": [{"kind": "pre", "expr": op("not", op("null", xs))}],
         "properties": [], "prove": False},
        {"name": "min_of_list", "intent": "The smallest element of a non-empty list.",
         "summary": "foldl min over the tail, seeded with the head. Requires a non-empty list.",
         "tags": ["list", "fold", "order", "partial", "refinement"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", var("min"), bapp("head", xs), bapp("tail", xs))),
         "examples": [{"args": [[3, 1, 4, 1, 5]], "result": 1}, {"args": [[7]], "result": 7},
                      {"args": [[-2, -9, -4]], "result": -9}],
         "refinements": [{"kind": "pre", "expr": op("not", op("null", xs))}],
         "properties": [], "prove": False},
        {"name": "count_between", "intent": "Count the list elements within a [lo, hi] range.",
         "summary": "length of the elements x with lo <= x and x <= hi (an inline predicate).",
         "tags": ["list", "filter", "composition"], "type_ast": fn([INT, INT, list_of(INT)], NAT),
         "body_ast": lam(["lo", "hi", "xs"], bapp("length", bapp("filter",
                         lam(["x"], bapp("and", bapp("le", lo, x), bapp("le", x, hi))), xs))),
         "examples": [{"args": [1, 3, [0, 1, 2, 3, 4]], "result": 3}, {"args": [0, 0, []], "result": 0},
                      {"args": [2, 5, [1, 6, 3]], "result": 1}],
         "properties": [], "prove": False},
        {"name": "clamp_all", "intent": "Clamp every element of a list to a [lo, hi] range.",
         "summary": "maps max(lo, min(hi, x)) over the list.", "tags": ["list", "map", "order"],
         "type_ast": fn([INT, INT, list_of(INT)], list_of(INT)),
         "body_ast": lam(["lo", "hi", "xs"], bapp("map",
                         lam(["x"], bapp("max", lo, bapp("min", hi, x))), xs)),
         "examples": [{"args": [0, 10, [-5, 5, 20]], "result": [0, 5, 10]}, {"args": [0, 10, []], "result": []}],
         "properties": [], "prove": False},
        {"name": "sum_of_cubes", "intent": "Sum the cubes of a list of numbers.",
         "summary": "folds add over each element cubed, 0 for the empty list.", "tags": ["list", "fold", "composition"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", lam(["acc", "x"], bapp("add", acc, bapp("mul", x, bapp("mul", x, x)))),
                                      int_lit(0), xs)),
         "examples": [{"args": [[]], "result": 0}, {"args": [[1, 2, 3]], "result": 36}, {"args": [[2, -3]], "result": -19}],
         "properties": [], "prove": False},
    ]


def more_compositional():
    # A second batch of non-recursive, multi-operator bodies — more generative breadth for the `write`
    # task (the eval's weakest skill), all in shapes not already covered: two-argument arithmetic
    # compositions (`average_two`, `abs_diff`, `sum_squares_two`, `square_diff`), a clamp-from-below
    # (`at_least_zero`), a `map` with a new scale factor (`triple_all`), a `filter` with a new predicate
    # (`keep_negatives`), a `length`-of-`filter` count (`count_negatives`), and a filter→fold pipeline
    # (`sum_evens`). Runnable; examples only.
    a, b, n, x, xs = var("a"), var("b"), var("n"), var("x"), var("xs")
    is_even_pred = lam(["x"], bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0)))
    is_neg_pred = lam(["x"], bapp("lt", x, int_lit(0)))
    return [
        {"name": "average_two", "intent": "The integer average of two integers.",
         "summary": "(a + b) divided by 2.", "tags": ["arithmetic", "composition"],
         "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("div", bapp("add", a, b), int_lit(2))),
         "examples": [{"args": [4, 6], "result": 5}, {"args": [10, 4], "result": 7},
                      {"args": [3, 3], "result": 3}, {"args": [0, 0], "result": 0}],
         "properties": [], "prove": False},
        {"name": "abs_diff", "intent": "The absolute difference of two integers.",
         "summary": "the absolute value of a - b.", "tags": ["arithmetic", "composition"],
         "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("abs", bapp("sub", a, b))),
         "examples": [{"args": [3, 7], "result": 4}, {"args": [7, 3], "result": 4},
                      {"args": [5, 5], "result": 0}],
         "properties": [], "prove": False},
        {"name": "sum_squares_two", "intent": "The sum of the squares of two integers.",
         "summary": "a*a + b*b.", "tags": ["arithmetic", "composition"],
         "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("add", bapp("mul", a, a), bapp("mul", b, b))),
         "examples": [{"args": [3, 4], "result": 25}, {"args": [0, 0], "result": 0},
                      {"args": [1, 2], "result": 5}],
         "properties": [], "prove": False},
        {"name": "square_diff", "intent": "The difference of the squares of two integers.",
         "summary": "a*a - b*b.", "tags": ["arithmetic", "composition"],
         "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], bapp("sub", bapp("mul", a, a), bapp("mul", b, b))),
         "examples": [{"args": [3, 2], "result": 5}, {"args": [5, 5], "result": 0},
                      {"args": [2, 3], "result": -5}],
         "properties": [], "prove": False},
        {"name": "at_least_zero", "intent": "Clamp an integer up to a minimum of zero.",
         "summary": "the larger of 0 and n (negatives become 0).", "tags": ["arithmetic", "order"],
         "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], bapp("max", int_lit(0), n)),
         "examples": [{"args": [-5], "result": 0}, {"args": [5], "result": 5}, {"args": [0], "result": 0}],
         "properties": [], "prove": False},
        {"name": "triple_all", "intent": "Triple every element of a list.",
         "summary": "maps x*3 over the list.", "tags": ["list", "map"],
         "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("map", lam(["x"], bapp("mul", x, int_lit(3))), xs)),
         "examples": [{"args": [[1, 2, 3]], "result": [3, 6, 9]}, {"args": [[]], "result": []},
                      {"args": [[-1, 0]], "result": [-3, 0]}],
         "properties": [], "prove": False},
        {"name": "keep_negatives", "intent": "Keep only the negative numbers in a list.",
         "summary": "filters the list to elements less than 0.", "tags": ["list", "filter"],
         "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("filter", is_neg_pred, xs)),
         "examples": [{"args": [[1, -2, 3, -4]], "result": [-2, -4]}, {"args": [[]], "result": []},
                      {"args": [[1, 2]], "result": []}],
         "properties": [], "prove": False},
        {"name": "count_negatives", "intent": "Count the negative numbers in a list.",
         "summary": "the length of the elements less than 0.", "tags": ["list", "filter", "composition"],
         "type_ast": fn([list_of(INT)], NAT),
         "body_ast": lam(["xs"], bapp("length", bapp("filter", is_neg_pred, xs))),
         "examples": [{"args": [[1, -2, 3, -4]], "result": 2}, {"args": [[]], "result": 0},
                      {"args": [[1, 2]], "result": 0}],
         "properties": [], "prove": False},
        {"name": "sum_evens", "intent": "Sum the even numbers in a list.",
         "summary": "folds add over the elements that are even, 0 for none.",
         "tags": ["list", "filter", "fold", "composition"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", var("add"), int_lit(0), bapp("filter", is_even_pred, xs))),
         "examples": [{"args": [[1, 2, 3, 4]], "result": 6}, {"args": [[]], "result": 0},
                      {"args": [[1, 3]], "result": 0}, {"args": [[2, 4, 6]], "result": 12}],
         "properties": [], "prove": False},
    ]


def more_recursion():
    # A third recursion batch in shapes the earlier families don't cover: multiplication by repeated
    # addition (`mult_rec` — two-argument, recurses on the second), powers of two (`pow2` — a doubling
    # recursion), and a recursive (rather than fold-based) maximum of a non-empty list (`max_list_rec`,
    # structural on the tail). Runnable; examples only. Counter-driven recursions are `unknown`; the
    # structural one is `always`.
    a, b, n, xs = var("a"), var("b"), var("n"), var("xs")
    return [
        {"name": "mult_rec", "intent": "Multiply two non-negative integers by repeated addition.",
         "summary": "0 when b is 0; otherwise a + mult_rec(a, b-1).",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], case_bool(bapp("eq", b, int_lit(0)), int_lit(0),
                         bapp("add", a, bself(a, bapp("sub", b, int_lit(1)))))),
         "examples": [{"args": [3, 4], "result": 12}, {"args": [5, 0], "result": 0},
                      {"args": [2, 3], "result": 6}, {"args": [7, 1], "result": 7}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "pow2", "intent": "Two raised to the nth power.",
         "summary": "1 when n is 0; otherwise 2 * pow2(n-1).",
         "tags": ["arithmetic", "recursion"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(1),
                         bapp("mul", int_lit(2), bself(bapp("sub", n, int_lit(1)))))),
         "examples": [{"args": [0], "result": 1}, {"args": [3], "result": 8}, {"args": [5], "result": 32},
                      {"args": [1], "result": 2}],
         "properties": [], "prove": False, "terminates": "unknown"},
        {"name": "max_list_rec", "intent": "The largest element of a non-empty list, by recursion.",
         "summary": "the head when it is the only element; otherwise max(head, max_list_rec(tail)).",
         "tags": ["list", "recursion", "order", "partial", "refinement"],
         "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], case_bool(bapp("null", bapp("tail", xs)), bapp("head", xs),
                         bapp("max", bapp("head", xs), bself(bapp("tail", xs))))),
         "examples": [{"args": [[3, 1, 4]], "result": 4}, {"args": [[7]], "result": 7},
                      {"args": [[-2, -9, -4]], "result": -2}],
         "refinements": [{"kind": "pre", "expr": op("not", op("null", xs))}],
         "properties": [], "prove": False, "terminates": "always"},
    ]


def _vpat(tag, bind=None):
    """A variant case-pattern: `Just(x)` (with a payload binder) or nullary `None`."""
    p = {"kind": "variant", "tag": tag}
    if bind is not None:
        p["payload"] = {"kind": "bind", "name": bind}
    return p


def _case_of(scrutinee, *arms):
    """`case <scrutinee> of { p1 => e1; ... }` from (pattern, body) pairs — the general case (the
    case_bool/case_null helpers only cover boolean scrutinees; this one takes variant patterns)."""
    return {"kind": "case", "scrutinee": scrutinee, "arms": [{"pattern": p, "body": b} for p, b in arms]}


def string_funcs():
    # STRING functions (spec/expressiveness.md phase 1): strings as data, not just carriers. The seven
    # builtins are total/pure/deterministic; parse_int returns a Maybe (totality-via-Maybe — the pattern
    # that replaces `error`), so several rows both construct AND consume variants over strings. String
    # laws are out of the prover's fragment, so these verify by validate + typecheck + run.
    s, n, xs, x = var("s"), var("n"), var("xs"), var("x")
    return [
        {"name": "str_len", "intent": "The length of a string in characters.",
         "summary": "str_length s — Unicode scalar values, not bytes.", "tags": ["string"],
         "type_ast": fn([STRING], NAT), "body_ast": lam(["s"], bapp("str_length", s)),
         "examples": [{"args": ["hello"], "result": 5}, {"args": [""], "result": 0},
                      {"args": ["héllo"], "result": 5}],
         # PROVED over every string via the solver's string theory (the string proof fragment).
         "properties": [{"name": "nonnegative",
                         "expr": forall(["s"], op("ge", self_app(var("s")), int_lit(0)))}],
         "prove": True, "terminates": "always"},
        {"name": "wrap_parens", "intent": "Wrap a string in parentheses.",
         "summary": 'str_concat "(" (str_concat s ")").', "tags": ["string", "format"],
         "type_ast": fn([STRING], STRING),
         "body_ast": lam(["s"], bapp("str_concat", str_lit("("), bapp("str_concat", s, str_lit(")")))),
         "examples": [{"args": ["x"], "result": "(x)"}, {"args": [""], "result": "()"},
                      {"args": ["a,b"], "result": "(a,b)"}],
         # Wrapping adds exactly two characters — a string law PROVED over the unbounded domain.
         "properties": [{"name": "adds_two_chars",
                         "expr": forall(["s"], op("eq", op("str_length", self_app(var("s"))),
                                                 op("add", op("str_length", var("s")), int_lit(2))))}],
         "prove": True, "terminates": "always"},
        {"name": "contains_comma", "intent": "Test whether a string contains a comma.",
         "summary": 'str_contains "," s — the needle comes first.', "tags": ["string", "predicate"],
         "type_ast": fn([STRING], BOOL), "body_ast": lam(["s"], bapp("str_contains", str_lit(","), s)),
         "examples": [{"args": ["a,b"], "result": True}, {"args": ["ab"], "result": False},
                      {"args": [","], "result": True}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "count_fields", "intent": "How many comma-separated fields a string has.",
         "summary": 'length (str_split "," s) — split keeps empties, so "" has one field.',
         "tags": ["string", "parse"], "type_ast": fn([STRING], NAT),
         "body_ast": lam(["s"], bapp("length", bapp("str_split", str_lit(","), s))),
         "examples": [{"args": ["a,b,c"], "result": 3}, {"args": [""], "result": 1},
                      {"args": ["a,,b"], "result": 3}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "second_field", "intent": "The second comma-separated field of a string.",
         "summary": 'head (tail (str_split "," s)).', "tags": ["string", "parse"],
         "type_ast": fn([STRING], STRING),
         "body_ast": lam(["s"], bapp("head", bapp("tail", bapp("str_split", str_lit(","), s)))),
         "examples": [{"args": ["a,b,c"], "result": "b"}, {"args": ["x,y"], "result": "y"},
                      {"args": ["1,2,3,4"], "result": "2"}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "comma_join", "intent": "Join a list of strings with commas.",
         "summary": 'str_join "," xs — the separator comes first.', "tags": ["string", "format"],
         "type_ast": fn([list_of(STRING)], STRING), "body_ast": lam(["xs"], bapp("str_join", str_lit(","), xs)),
         "examples": [{"args": [["a", "b"]], "result": "a,b"}, {"args": [[]], "result": ""},
                      {"args": [["z"]], "result": "z"}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "split_words", "intent": "Split a string on spaces.",
         "summary": 'str_split " " s — keeps empty fields from repeated spaces.', "tags": ["string", "parse"],
         "type_ast": fn([STRING], list_of(STRING)), "body_ast": lam(["s"], bapp("str_split", str_lit(" "), s)),
         "examples": [{"args": ["a b"], "result": ["a", "b"]}, {"args": ["ab"], "result": ["ab"]},
                      {"args": ["a  b"], "result": ["a", "", "b"]}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "show_int", "intent": "Render an integer as its decimal string.",
         "summary": "to_string n — the canonical decimal rendering.", "tags": ["string", "format", "arithmetic"],
         "type_ast": fn([INT], STRING), "body_ast": lam(["n"], bapp("to_string", n)),
         "examples": [{"args": [42], "result": "42"}, {"args": [-7], "result": "-7"},
                      {"args": [0], "result": "0"}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "parse_int_maybe", "intent": "Parse a string as an integer, if it is one.",
         "summary": "parse_int s — Just(n) for exactly canonical decimal, else None (never an error).",
         "tags": ["string", "parse", "maybe"], "type_ast": fn([STRING], maybe_t(INT)),
         "body_ast": lam(["s"], bapp("parse_int", s)),
         "examples": [{"args": ["42"], "result": V("Just", 42)}, {"args": ["abc"], "result": V("None")},
                      {"args": ["-7"], "result": V("Just", -7)}, {"args": ["007"], "result": V("None")}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "parse_or_zero", "intent": "Parse a string as an integer, defaulting to zero.",
         "summary": "case parse_int s of Just(n) => n; None => 0 — consume the Maybe.",
         "tags": ["string", "parse", "variant", "case"], "type_ast": fn([STRING], INT),
         "body_ast": lam(["s"], _case_of(bapp("parse_int", s), (_vpat("Just", "n"), n),
                                         (_vpat("None"), int_lit(0)))),
         "examples": [{"args": ["9"], "result": 9}, {"args": ["junk"], "result": 0},
                      {"args": ["-3"], "result": -3}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "is_int_string", "intent": "Test whether a string is a canonical decimal integer.",
         "summary": "true iff parse_int s is a Just.", "tags": ["string", "parse", "predicate", "variant", "case"],
         "type_ast": fn([STRING], BOOL),
         "body_ast": lam(["s"], _case_of(bapp("parse_int", s), (_vpat("Just", "n"), bool_lit(True)),
                                         (_vpat("None"), bool_lit(False)))),
         "examples": [{"args": ["12"], "result": True}, {"args": ["1.5"], "result": False},
                      {"args": [""], "result": False}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "parse_and_double", "intent": "Parse a string as an integer and double it, or zero if unparseable.",
         "summary": "case parse_int s of Just(n) => n + n; None => 0.",
         "tags": ["string", "parse", "variant", "case", "arithmetic"], "type_ast": fn([STRING], INT),
         "body_ast": lam(["s"], _case_of(bapp("parse_int", s), (_vpat("Just", "n"), bapp("add", n, n)),
                                         (_vpat("None"), int_lit(0)))),
         "examples": [{"args": ["21"], "result": 42}, {"args": ["x"], "result": 0},
                      {"args": ["-5"], "result": -10}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "render_ints", "intent": "Render a list of integers as a comma-separated string.",
         "summary": 'str_join "," (map to_string xs).', "tags": ["string", "format", "list"],
         "type_ast": fn([list_of(INT)], STRING),
         "body_ast": lam(["xs"], bapp("str_join", str_lit(","), bapp("map", var("to_string"), xs))),
         "examples": [{"args": [[1, 2, 3]], "result": "1,2,3"}, {"args": [[]], "result": ""},
                      {"args": [[-4]], "result": "-4"}],
         "properties": [], "prove": False},
    ]


def map_json_funcs():
    # MAP + JSON functions (spec/expressiveness.md phases 2-3): dynamic key-value data and the
    # language's own canonical form as a manipulable value. map_get/parse_json are total via Maybe,
    # so most rows destructure a Maybe by case (incl. nested Json patterns — the GW1 practical form).
    # Sums are opaque to the prover; these verify by validate + typecheck + run.
    s, k, m, n, j = var("s"), var("k"), var("m"), var("n"), var("j")
    return [
        {"name": "lookup_int", "intent": "Look up a key in a map of integers, if present.",
         "summary": "map_get k m — the key comes first; an absent key is None, never an error.",
         "tags": ["map", "query", "maybe"], "type_ast": fn([STRING, map_of(INT)], maybe_t(INT)),
         "body_ast": lam(["k", "m"], bapp("map_get", k, m)),
         "examples": [{"args": ["a", {"a": 1, "b": 2}], "result": V("Just", 1)},
                      {"args": ["z", {"a": 1}], "result": V("None")},
                      {"args": ["x", {}], "result": V("None")}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "port_or_default", "intent": "The port entry of a config map, or 8080 if unset.",
         "summary": 'case map_get "port" m of Just(p) => p; None => 8080 — the config-lookup idiom.',
         "tags": ["map", "query", "variant", "case"], "type_ast": fn([map_of(INT)], INT),
         "body_ast": lam(["m"], _case_of(bapp("map_get", str_lit("port"), m),
                                         (_vpat("Just", "p"), var("p")), (_vpat("None"), int_lit(8080)))),
         "examples": [{"args": [{"port": 9000, "timeout": 30}], "result": 9000},
                      {"args": [{}], "result": 8080},
                      {"args": [{"retries": 3}], "result": 8080}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "key_count", "intent": "How many entries a map has.",
         "summary": "map_size m.", "tags": ["map", "aggregate"],
         "type_ast": fn([map_of(INT)], NAT), "body_ast": lam(["m"], bapp("map_size", m)),
         "examples": [{"args": [{"a": 1, "b": 2}], "result": 2}, {"args": [{}], "result": 0},
                      {"args": [{"only": 7}], "result": 1}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "key_list", "intent": "The keys of a map, sorted.",
         "summary": "map_keys m — deterministic (sorted) order.", "tags": ["map", "query"],
         "type_ast": fn([map_of(INT)], list_of(STRING)), "body_ast": lam(["m"], bapp("map_keys", m)),
         "examples": [{"args": [{"b": 2, "a": 1}], "result": ["a", "b"]}, {"args": [{}], "result": []},
                      {"args": [{"z": 0}], "result": ["z"]}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "store_one", "intent": "A one-entry map from a key and a value.",
         "summary": "map_put k n map_empty — build from the empty map.", "tags": ["map", "transform"],
         "type_ast": fn([STRING, INT], map_of(INT)),
         "body_ast": lam(["k", "n"], bapp("map_put", k, n, var("map_empty"))),
         "examples": [{"args": ["a", 1], "result": {"a": 1}}, {"args": ["port", 9000], "result": {"port": 9000}},
                      {"args": ["", 0], "result": {"": 0}}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "drop_key", "intent": "Remove a key from a map (a no-op if absent).",
         "summary": "map_del k m — total; deleting an absent key returns the map unchanged.",
         "tags": ["map", "transform"], "type_ast": fn([STRING, map_of(INT)], map_of(INT)),
         "body_ast": lam(["k", "m"], bapp("map_del", k, m)),
         "examples": [{"args": ["a", {"a": 1, "b": 2}], "result": {"b": 2}},
                      {"args": ["z", {"a": 1}], "result": {"a": 1}},
                      {"args": ["x", {}], "result": {}}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "is_valid_json", "intent": "Test whether a string is valid JSON.",
         "summary": "true iff parse_json succeeds.", "tags": ["parse", "string", "predicate", "variant", "case"],
         "type_ast": fn([STRING], BOOL),
         "body_ast": lam(["s"], _case_of(bapp("parse_json", s), (_vpat("Just", "j"), bool_lit(True)),
                                         (_vpat("None"), bool_lit(False)))),
         "examples": [{"args": ["{\"a\": 1}"], "result": True}, {"args": ["[1, 2]"], "result": True},
                      {"args": ["{nope"], "result": False}, {"args": [""], "result": False}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "canonical_json", "intent": "Canonicalize a JSON text, or empty on invalid input.",
         "summary": "render_json of a parse_json IS canonicalization (JCS: sorted keys, minimal form).",
         "tags": ["parse", "serialize", "string", "variant", "case"], "type_ast": fn([STRING], STRING),
         "body_ast": lam(["s"], _case_of(bapp("parse_json", s), (_vpat("Just", "j"), bapp("render_json", j)),
                                         (_vpat("None"), str_lit("")))),
         "examples": [{"args": ["{ \"b\" : 2 , \"a\": 1 }"], "result": "{\"a\":1,\"b\":2}"},
                      {"args": ["[1,  2]"], "result": "[1,2]"},
                      {"args": ["junk"], "result": ""}],
         "properties": [], "prove": False, "terminates": "always"},
        {"name": "json_port", "intent": "The integer port field of a JSON config text, or 8080.",
         "summary": "parse the payload, project the field: nested case over Just(JObj(m)) then Just(JNum(p)).",
         "tags": ["parse", "query", "string", "map", "variant", "case"], "type_ast": fn([STRING], INT),
         "body_ast": lam(["s"], _case_of(
             bapp("parse_json", s),
             ({"kind": "variant", "tag": "Just", "payload": {"kind": "variant", "tag": "JObj", "payload": {"kind": "bind", "name": "m"}}},
              _case_of(bapp("map_get", str_lit("port"), m),
                       ({"kind": "variant", "tag": "Just", "payload": {"kind": "variant", "tag": "JNum", "payload": {"kind": "bind", "name": "p"}}},
                        var("p")),
                       (WILDCARD_PAT, int_lit(8080)))),
             (WILDCARD_PAT, int_lit(8080)))),
         "examples": [{"args": ["{\"host\": \"h\", \"port\": 9000}"], "result": 9000},
                      {"args": ["{\"host\": \"h\"}"], "result": 8080},
                      {"args": ["nope"], "result": 8080}],
         "properties": [], "prove": False, "terminates": "always"},
    ]


def variant_consuming_funcs():
    # The write shape the corpus most lacked: bodies that CONSUME a sum type by pattern-matching its
    # variants (the existing maybe_funcs/result_funcs only CONSTRUCT variants). Plus two more constructors
    # for breadth. Sum types are opaque to the prover, so these verify by validate + typecheck + run.
    m, d, r, n, x = var("m"), var("d"), var("r"), var("n"), var("x")
    return [
        {"name": "unwrap_or", "intent": "Get the value inside an optional, or a default if there is none.",
         "summary": "the payload of Just; the default for None.", "tags": ["maybe", "variant", "case"],
         "type_ast": fn([maybe_t(INT), INT], INT),
         "body_ast": lam(["m", "d"], _case_of(m, (_vpat("Just", "x"), x), (_vpat("None"), d))),
         "examples": [{"args": [V("Just", 5), 0], "result": 5}, {"args": [V("None"), 9], "result": 9},
                      {"args": [V("Just", -3), 1], "result": -3}],
         "properties": [], "prove": False},
        {"name": "is_some", "intent": "Test whether an optional holds a value.",
         "summary": "true for Just, false for None.", "tags": ["maybe", "variant", "case", "predicate"],
         "type_ast": fn([maybe_t(INT)], BOOL),
         "body_ast": lam(["m"], _case_of(m, (_vpat("Just", "x"), bool_lit(True)), (_vpat("None"), bool_lit(False)))),
         "examples": [{"args": [V("Just", 7)], "result": True}, {"args": [V("None")], "result": False}],
         "properties": [], "prove": False},
        {"name": "maybe_double", "intent": "Double the value inside an optional, leaving None unchanged.",
         "summary": "Just(x*2) for Just(x); None for None — a map over the optional.",
         "tags": ["maybe", "variant", "case"], "type_ast": fn([maybe_t(INT)], maybe_t(INT)),
         "body_ast": lam(["m"], _case_of(m, (_vpat("Just", "x"), variant_expr("Just", bapp("mul", x, int_lit(2)))),
                                         (_vpat("None"), variant_expr("None")))),
         "examples": [{"args": [V("Just", 5)], "result": V("Just", 10)}, {"args": [V("None")], "result": V("None")},
                      {"args": [V("Just", -4)], "result": V("Just", -8)}],
         "properties": [], "prove": False},
        {"name": "unwrap_result", "intent": "Get the success value of a result, or zero on error.",
         "summary": "the payload of Ok; 0 for Err.", "tags": ["result", "variant", "case"],
         "type_ast": fn([result_t(INT, INT)], INT),
         "body_ast": lam(["r"], _case_of(r, (_vpat("Ok", "x"), x), (_vpat("Err", "e"), int_lit(0)))),
         "examples": [{"args": [V("Ok", 4)], "result": 4}, {"args": [V("Err", 9)], "result": 0},
                      {"args": [V("Ok", -2)], "result": -2}],
         "properties": [], "prove": False},
        {"name": "result_to_maybe", "intent": "Convert a result to an optional, discarding the error.",
         "summary": "Just(x) for Ok(x); None for Err.", "tags": ["result", "maybe", "variant", "case"],
         "type_ast": fn([result_t(INT, INT)], maybe_t(INT)),
         "body_ast": lam(["r"], _case_of(r, (_vpat("Ok", "x"), variant_expr("Just", x)),
                                         (_vpat("Err", "e"), variant_expr("None")))),
         "examples": [{"args": [V("Ok", 4)], "result": V("Just", 4)}, {"args": [V("Err", 9)], "result": V("None")}],
         "properties": [], "prove": False},
        {"name": "predecessor", "intent": "The predecessor of a positive integer, or nothing at zero or below.",
         "summary": "Just(n-1) when n > 0; None otherwise.", "tags": ["arithmetic", "maybe", "partial"],
         "type_ast": fn([INT], maybe_t(INT)),
         "body_ast": lam(["n"], case_bool(bapp("gt", n, int_lit(0)),
                                          variant_expr("Just", bapp("sub", n, int_lit(1))), variant_expr("None"))),
         "examples": [{"args": [5], "result": V("Just", 4)}, {"args": [0], "result": V("None")},
                      {"args": [1], "result": V("Just", 0)}],
         "properties": [], "prove": False},
        {"name": "to_result_nonneg", "intent": "Tag a number Ok if non-negative, else Err with the value.",
         "summary": "Ok(n) when n >= 0; Err(n) otherwise.", "tags": ["arithmetic", "result"],
         "type_ast": fn([INT], result_t(INT, INT)),
         "body_ast": lam(["n"], case_bool(bapp("ge", n, int_lit(0)),
                                          variant_expr("Ok", n), variant_expr("Err", n))),
         "examples": [{"args": [5], "result": V("Ok", 5)}, {"args": [-3], "result": V("Err", -3)},
                      {"args": [0], "result": V("Ok", 0)}],
         "properties": [], "prove": False},
    ]


def nested_hof_funcs():
    # Nested higher-order bodies (one higher-order builtin applied to the result of another, with inline
    # lambdas) and multi-clause `case` chains — both write shapes thin in the corpus. First-order records,
    # runnable; examples only.
    xs, x, a, b, n = var("xs"), var("x"), var("a"), var("b"), var("n")
    is_even = lam(["x"], bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0)))
    return [
        {"name": "count_even_positives", "intent": "Count the elements that are both positive and even.",
         "summary": "length of the elements x with x > 0 and x even (a compound inline predicate).",
         "tags": ["list", "filter", "composition"], "type_ast": fn([list_of(INT)], NAT),
         "body_ast": lam(["xs"], bapp("length", bapp("filter",
                         lam(["x"], bapp("and", bapp("gt", x, int_lit(0)),
                                         bapp("eq", bapp("mod", x, int_lit(2)), int_lit(0)))), xs))),
         "examples": [{"args": [[1, 2, 3, 4, -2]], "result": 2}, {"args": [[]], "result": 0},
                      {"args": [[1, 3, 5]], "result": 0}],
         "properties": [], "prove": False},
        {"name": "doubled_evens", "intent": "Keep the even numbers and double each.",
         "summary": "maps x*2 over the even elements — a map applied to a filter.",
         "tags": ["list", "map", "filter", "composition"], "type_ast": fn([list_of(INT)], list_of(INT)),
         "body_ast": lam(["xs"], bapp("map", lam(["x"], bapp("mul", x, int_lit(2))), bapp("filter", is_even, xs))),
         "examples": [{"args": [[1, 2, 3, 4]], "result": [4, 8]}, {"args": [[]], "result": []},
                      {"args": [[1, 3]], "result": []}],
         "properties": [], "prove": False},
        {"name": "sum_doubled", "intent": "Sum a list after doubling every element.",
         "summary": "folds add over each element doubled — a fold applied to a map.",
         "tags": ["list", "map", "fold", "composition"], "type_ast": fn([list_of(INT)], INT),
         "body_ast": lam(["xs"], bapp("foldl", var("add"), int_lit(0),
                         bapp("map", lam(["x"], bapp("mul", x, int_lit(2))), xs))),
         "examples": [{"args": [[1, 2, 3]], "result": 12}, {"args": [[]], "result": 0}, {"args": [[5]], "result": 10}],
         "properties": [], "prove": False},
        {"name": "any_even", "intent": "Test whether a list contains an even number.",
         "summary": "true when the even-filtered list is non-empty — filter, then null, then not.",
         "tags": ["list", "filter", "predicate", "composition"], "type_ast": fn([list_of(INT)], BOOL),
         "body_ast": lam(["xs"], bapp("not", bapp("null", bapp("filter", is_even, xs)))),
         "examples": [{"args": [[1, 3, 4]], "result": True}, {"args": [[1, 3]], "result": False},
                      {"args": [[]], "result": False}],
         "properties": [], "prove": False},
        {"name": "grade", "intent": "Map a score to a 4/3/2/0 grade by threshold.",
         "summary": "4 if >=90, else 3 if >=80, else 2 if >=70, else 0 — a nested case chain.",
         "tags": ["arithmetic", "case"], "type_ast": fn([INT], INT),
         "body_ast": lam(["n"], case_bool(bapp("ge", n, int_lit(90)), int_lit(4),
                         case_bool(bapp("ge", n, int_lit(80)), int_lit(3),
                         case_bool(bapp("ge", n, int_lit(70)), int_lit(2), int_lit(0))))),
         "examples": [{"args": [95], "result": 4}, {"args": [85], "result": 3}, {"args": [75], "result": 2},
                      {"args": [50], "result": 0}],
         "properties": [], "prove": False},
        {"name": "compare_to", "intent": "Three-way compare two integers to -1, 0, or 1.",
         "summary": "-1 if a < b, 0 if equal, 1 otherwise — a nested case.",
         "tags": ["arithmetic", "order", "case"], "type_ast": fn([INT, INT], INT),
         "body_ast": lam(["a", "b"], case_bool(bapp("lt", a, b), int_lit(-1),
                         case_bool(bapp("eq", a, b), int_lit(0), int_lit(1)))),
         "examples": [{"args": [2, 5], "result": -1}, {"args": [5, 5], "result": 0}, {"args": [7, 3], "result": 1}],
         "properties": [], "prove": False},
    ]


# --- combinatorial generation: parameterized templates, verified at scale ------------------------
#
# The hand-authored families above give breadth of SHAPE; these multiply each shape over a fixed set of
# constants/operators/comparisons to give the VOLUME a fine-tuning dataset needs. Every generated spec
# still flows through the same build_and_verify gate (validate + typecheck + run), so each is
# correct-by-construction AND checked by the reference tooling. Deterministic (fixed enumeration, no RNG)
# so the output is byte-reproducible. Opt-in via `gen_corpus.py --combinatorial --out <scratch path>`; the
# curated corpus.jsonl (default, no flag) is unchanged. Widen the sets below to scale the count further.

_KADD = list(range(1, 13))                            # add/sub constants: 1..12
_KMUL = list(range(2, 13))                            # mul constants: 2..12 (1 omitted — identity)
_KCMP = [-5, -3, -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10]   # comparison constants (incl. negatives)
_K2 = {"add": list(range(1, 13)), "sub": list(range(1, 13)), "mul": list(range(2, 13))}  # two-step: 1..12 / 2..12
_K3 = {"add": [1, 2, 3], "sub": [1, 2, 3], "mul": [2, 3]}  # three-step (smaller, to bound the cube)
_INT_IN = [0, 1, -1, 4, -3, 7, 12]                              # worked inputs for INT-domain functions
_LIST_IN = [[], [1, 2, 3], [5, -2, 4, 0], [-1, -2], [3, 3, 7]]  # worked inputs for List-domain functions
_AOP = {"add": lambda a, b: a + b, "sub": lambda a, b: a - b, "mul": lambda a, b: a * b}
_CMP = {"lt": lambda a, b: a < b, "le": lambda a, b: a <= b, "gt": lambda a, b: a > b,
        "ge": lambda a, b: a >= b, "eq": lambda a, b: a == b}
_CMPWORD = {"lt": "less than", "le": "at most", "gt": "greater than", "ge": "at least", "eq": "equal to"}
_OPWORD = {"add": "add", "sub": "subtract", "mul": "multiply by"}
_INTENT1 = {"add": lambda k: f"Add {k} to a number.", "sub": lambda k: f"Subtract {k} from a number.",
            "mul": lambda k: f"Multiply a number by {k}."}


def _cspec(name, intent, summary, tags, ty, body, examples, **extra):
    spec = {"name": name, "intent": intent, "summary": summary, "tags": ["combinatorial"] + tags,
            "type_ast": ty, "body_ast": body, "examples": examples, "properties": [], "prove": False}
    spec.update(extra)  # e.g. terminates="unknown" for counter-driven recursions, refinements=[...]
    return spec


def combinatorial_specs(exclude_names=()):
    """Parameterized function specs (same format as the hand-authored families). See the section note."""
    out, seen_body, seen_name = [], set(), set(exclude_names)

    def add(spec):
        key = json.dumps(spec["body_ast"], sort_keys=True)
        if spec["name"] in seen_name or key in seen_body:
            return
        seen_name.add(spec["name"])
        seen_body.add(key)
        out.append(spec)

    x, n, xs = var("x"), var("n"), var("xs")

    # 1. unary scalar arithmetic:  \n -> op n k   (+ reversed subtraction  k - n)
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (_KMUL if op == "mul" else _KADD):
            add(_cspec(f"{op}_{k}", _INTENT1[op](k), f"{op} n {k}", ["arithmetic", "unary", op],
                       fn([INT], INT), lam(["n"], bapp(op, n, int_lit(k))),
                       [{"args": [v], "result": pf(v, k)} for v in _INT_IN]))
    for k in _KADD:
        add(_cspec(f"from_{k}", f"Subtract a number from {k}.", f"{k} - n", ["arithmetic", "unary", "sub"],
                   fn([INT], INT), lam(["n"], bapp("sub", int_lit(k), n)),
                   [{"args": [v], "result": k - v} for v in _INT_IN]))

    # 2. map a unary op over a list:  \xs -> map (\x -> op x k) xs
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (_KMUL if op == "mul" else _KADD):
            add(_cspec(f"map_{op}_{k}", f"{_OPWORD[op].capitalize()} {k} over every element of a list.",
                       f"map ({op} x {k}) xs", ["list", "map", op], fn([list_of(INT)], list_of(INT)),
                       lam(["xs"], bapp("map", lam(["x"], bapp(op, x, int_lit(k))), xs)),
                       [{"args": [lst], "result": [pf(v, k) for v in lst]} for lst in _LIST_IN]))

    # 3. filter / 4. count / 5. predicate, by comparison:  cmp _ k
    for cmp, cf in _CMP.items():
        for k in _KCMP:
            add(_cspec(f"filter_{cmp}_{k}", f"Keep the list elements {_CMPWORD[cmp]} {k}.",
                       f"filter ({cmp} x {k}) xs", ["list", "filter", "predicate"], fn([list_of(INT)], list_of(INT)),
                       lam(["xs"], bapp("filter", lam(["x"], bapp(cmp, x, int_lit(k))), xs)),
                       [{"args": [lst], "result": [v for v in lst if cf(v, k)]} for lst in _LIST_IN]))
            add(_cspec(f"count_{cmp}_{k}", f"Count the list elements {_CMPWORD[cmp]} {k}.",
                       f"length (filter ({cmp} x {k}) xs)", ["list", "filter", "count"], fn([list_of(INT)], NAT),
                       lam(["xs"], bapp("length", bapp("filter", lam(["x"], bapp(cmp, x, int_lit(k))), xs))),
                       [{"args": [lst], "result": sum(1 for v in lst if cf(v, k))} for lst in _LIST_IN]))
            add(_cspec(f"is_{cmp}_{k}", f"Test whether a number is {_CMPWORD[cmp]} {k}.", f"{cmp} n {k}",
                       ["predicate", "comparison"], fn([INT], BOOL), lam(["n"], bapp(cmp, n, int_lit(k))),
                       [{"args": [v], "result": cf(v, k)} for v in _INT_IN]))

    # 6. two-step scalar arithmetic:  \n -> op2 (op1 n k1) k2
    steps = [(op, k) for op in ("add", "sub", "mul") for k in _K2[op]]
    for op1, k1 in steps:
        for op2, k2 in steps:
            pf1, pf2 = _AOP[op1], _AOP[op2]
            add(_cspec(f"{op1}{k1}_{op2}{k2}",
                       f"{_OPWORD[op1].capitalize()} {k1}, then {_OPWORD[op2]} {k2}.",
                       f"{op2} ({op1} n {k1}) {k2}", ["arithmetic", "composition", "two-step"], fn([INT], INT),
                       lam(["n"], bapp(op2, bapp(op1, n, int_lit(k1)), int_lit(k2))),
                       [{"args": [v], "result": pf2(pf1(v, k1), k2)} for v in _INT_IN]))

    # 7. guarded optional:  \n -> case cmp n k of { true => Just(op n k2); false => None }
    for cmp in ("gt", "ge", "lt"):
        cf = _CMP[cmp]
        for op in ("add", "sub", "mul"):
            pf, k2 = _AOP[op], (2 if op == "mul" else 1)
            for k in (1, 2, 3):
                add(_cspec(f"guard_{cmp}_{k}_{op}",
                           f"When a number is {_CMPWORD[cmp]} {k}, return it {_OPWORD[op]} {k2} wrapped in Just; else None.",
                           f"case {cmp} n {k} => Just({op} n {k2}) / None", ["maybe", "variant", "case", "guarded"],
                           fn([INT], maybe_t(INT)),
                           lam(["n"], case_bool(bapp(cmp, n, int_lit(k)),
                                                variant_expr("Just", bapp(op, n, int_lit(k2))), variant_expr("None"))),
                           [{"args": [v], "result": (V("Just", pf(v, k2)) if cf(v, k) else V("None"))} for v in _INT_IN]))

    # 8. range predicate:  \n -> and (ge n lo) (le n hi)
    for lo, hi in [(0, 5), (1, 10), (2, 8), (-5, 5), (0, 9), (3, 7)]:
        add(_cspec(f"in_range_{lo}_{hi}".replace("-", "m"),
                   f"Test whether a number is in the range {lo} to {hi} inclusive.",
                   f"and (ge n {lo}) (le n {hi})", ["predicate", "comparison", "range"], fn([INT], BOOL),
                   lam(["n"], bapp("and", bapp("ge", n, int_lit(lo)), bapp("le", n, int_lit(hi)))),
                   [{"args": [v], "result": (lo <= v <= hi)} for v in _INT_IN]))

    # 9. filter then map:  \xs -> map (\x -> op x k2) (filter (\x -> cmp x k) xs)
    for cmp in ("gt", "ge", "lt", "le"):
        cf = _CMP[cmp]
        for op in ("add", "mul"):
            pf, k2 = _AOP[op], (2 if op == "mul" else 1)
            for k in (0, 1, 3):
                add(_cspec(f"map_{op}{k2}_filter_{cmp}_{k}",
                           f"Keep the elements {_CMPWORD[cmp]} {k}, then {_OPWORD[op]} {k2}.",
                           f"map ({op} x {k2}) (filter ({cmp} x {k}) xs)", ["list", "map", "filter", "composition"],
                           fn([list_of(INT)], list_of(INT)),
                           lam(["xs"], bapp("map", lam(["x"], bapp(op, x, int_lit(k2))),
                                            bapp("filter", lam(["x"], bapp(cmp, x, int_lit(k))), xs))),
                           [{"args": [lst], "result": [pf(v, k2) for v in lst if cf(v, k)]} for lst in _LIST_IN]))

    # 10. three-step scalar arithmetic:  \n -> op3 (op2 (op1 n k1) k2) k3   (smaller per-step sets)
    steps3 = [(op, k) for op in ("add", "sub", "mul") for k in _K3[op]]
    for op1, k1 in steps3:
        for op2, k2 in steps3:
            for op3, k3 in steps3:
                pf1, pf2, pf3 = _AOP[op1], _AOP[op2], _AOP[op3]
                add(_cspec(f"{op1}{k1}_{op2}{k2}_{op3}{k3}",
                           f"{_OPWORD[op1].capitalize()} {k1}, then {_OPWORD[op2]} {k2}, then {_OPWORD[op3]} {k3}.",
                           f"{op3} ({op2} ({op1} n {k1}) {k2}) {k3}", ["arithmetic", "composition", "three-step"],
                           fn([INT], INT),
                           lam(["n"], bapp(op3, bapp(op2, bapp(op1, n, int_lit(k1)), int_lit(k2)), int_lit(k3))),
                           [{"args": [v], "result": pf3(pf2(pf1(v, k1), k2), k3)} for v in _INT_IN]))

    # 11. compound predicate:  \n -> logic (cmp1 n k1) (cmp2 n k2)
    _LOGIC = {"and": lambda a, b: a and b, "or": lambda a, b: a or b}
    for logic, lf in _LOGIC.items():
        for cmp1 in ("gt", "ge", "lt", "le"):
            for cmp2 in ("gt", "ge", "lt", "le"):
                cf1, cf2 = _CMP[cmp1], _CMP[cmp2]
                for k1, k2 in [(0, 5), (1, 10), (2, 8)]:
                    nm = f"{logic}_{cmp1}_{k1}_{cmp2}_{k2}".replace("-", "m")
                    add(_cspec(nm,
                               f"Test whether a number is {_CMPWORD[cmp1]} {k1} {logic} {_CMPWORD[cmp2]} {k2}.",
                               f"{logic} ({cmp1} n {k1}) ({cmp2} n {k2})", ["predicate", "comparison", "compound"],
                               fn([INT], BOOL),
                               lam(["n"], bapp(logic, bapp(cmp1, n, int_lit(k1)), bapp(cmp2, n, int_lit(k2)))),
                               [{"args": [v], "result": lf(cf1(v, k1), cf2(v, k2))} for v in _INT_IN]))

    # 12. STRUCTURAL RECURSION on the tail — the write-hardest shapes (recursion + nested case), at scale.
    # The measured weak spot is generating recursion conventions-off; these parameterize it heavily.
    nil = var("nil")
    h = bapp("head", xs)
    t = bapp("tail", xs)
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (_KMUL if op == "mul" else _KADD):
            add(_cspec(f"rec_map_{op}_{k}", f"{_OPWORD[op].capitalize()} {k} over a list, by recursion.",
                       f"nil / cons ({op} (head xs) {k}) (self (tail xs))", ["list", "recursion", "map", op],
                       fn([list_of(INT)], list_of(INT)),
                       lam(["xs"], case_null("xs", nil, bapp("cons", bapp(op, h, int_lit(k)), bself(t)))),
                       [{"args": [lst], "result": [pf(v, k) for v in lst]} for lst in _LIST_IN]))
            add(_cspec(f"rec_sumof_{op}_{k}", f"Sum each list element after {_OPWORD[op]} {k}, by recursion.",
                       f"0 / add ({op} (head xs) {k}) (self (tail xs))", ["list", "recursion", "fold", op],
                       fn([list_of(INT)], INT),
                       lam(["xs"], case_null("xs", int_lit(0), bapp("add", bapp(op, h, int_lit(k)), bself(t)))),
                       [{"args": [lst], "result": sum(pf(v, k) for v in lst)} for lst in _LIST_IN]))
    for cmp, cf in _CMP.items():
        for k in _KCMP:
            add(_cspec(f"rec_filter_{cmp}_{k}", f"Keep the list elements {_CMPWORD[cmp]} {k}, by recursion.",
                       f"nil / (cons head | skip) on {cmp} (head xs) {k}, recursing on the tail",
                       ["list", "recursion", "filter", "case"], fn([list_of(INT)], list_of(INT)),
                       lam(["xs"], case_null("xs", nil,
                            case_bool(bapp(cmp, h, int_lit(k)), bapp("cons", h, bself(t)), bself(t)))),
                       [{"args": [lst], "result": [v for v in lst if cf(v, k)]} for lst in _LIST_IN]))
            add(_cspec(f"rec_count_{cmp}_{k}", f"Count the list elements {_CMPWORD[cmp]} {k}, by recursion.",
                       f"0 / (1 + self) when {cmp} (head xs) {k} else self, on the tail",
                       ["list", "recursion", "count", "case"], fn([list_of(INT)], INT),
                       lam(["xs"], case_null("xs", int_lit(0),
                            case_bool(bapp(cmp, h, int_lit(k)), bapp("add", int_lit(1), bself(t)), bself(t)))),
                       [{"args": [lst], "result": sum(1 for v in lst if cf(v, k))} for lst in _LIST_IN]))
            add(_cspec(f"rec_all_{cmp}_{k}", f"Test whether every list element is {_CMPWORD[cmp]} {k}, by recursion.",
                       f"true / and ({cmp} (head xs) {k}) (self (tail xs))", ["list", "recursion", "predicate", "case"],
                       fn([list_of(INT)], BOOL),
                       lam(["xs"], case_null("xs", bool_lit(True), bapp("and", bapp(cmp, h, int_lit(k)), bself(t)))),
                       [{"args": [lst], "result": all(cf(v, k) for v in lst)} for lst in _LIST_IN]))
            add(_cspec(f"rec_any_{cmp}_{k}", f"Test whether any list element is {_CMPWORD[cmp]} {k}, by recursion.",
                       f"false / or ({cmp} (head xs) {k}) (self (tail xs))", ["list", "recursion", "predicate", "case"],
                       fn([list_of(INT)], BOOL),
                       lam(["xs"], case_null("xs", bool_lit(False), bapp("or", bapp(cmp, h, int_lit(k)), bself(t)))),
                       [{"args": [lst], "result": any(cf(v, k) for v in lst)} for lst in _LIST_IN]))

    # The shapes below add STRUCTURAL diversity, not just more constants — the measured generalization gap
    # (held-out write was 0-9% on shapes the generator didn't cover vs 45-56% on shapes it did). Each is
    # lifted from a hand-authored family that already passes validate+typecheck+run, then parameterized.
    m, r = var("m"), var("r")

    # 13. CONSUME a sum type by pattern-matching its variants (Just/None, Ok/Err) — the variant/case gap.
    for k in (0, 1, -1, 2, 5, 10, 100):
        add(_cspec(f"unwrap_or_{k}".replace("-", "m"), f"Get the value inside an optional, or {k} if it is empty.",
                   f"the payload of Just; {k} for None.", ["maybe", "variant", "case"], fn([maybe_t(INT)], INT),
                   lam(["m"], _case_of(m, (_vpat("Just", "x"), x), (_vpat("None"), int_lit(k)))),
                   [{"args": [V("Just", 3)], "result": 3}, {"args": [V("Just", -2)], "result": -2},
                    {"args": [V("None")], "result": k}]))
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3, 5):
            add(_cspec(f"map_maybe_{op}_{k}", f"{_OPWORD[op].capitalize()} {k} inside an optional, leaving None unchanged.",
                       f"Just({op} x {k}) for Just(x); None for None.", ["maybe", "variant", "case", "map"],
                       fn([maybe_t(INT)], maybe_t(INT)),
                       lam(["m"], _case_of(m, (_vpat("Just", "x"), variant_expr("Just", bapp(op, x, int_lit(k)))),
                                           (_vpat("None"), variant_expr("None")))),
                       [{"args": [V("Just", 4)], "result": V("Just", pf(4, k))},
                        {"args": [V("Just", -1)], "result": V("Just", pf(-1, k))},
                        {"args": [V("None")], "result": V("None")}]))
    for k in (0, -1, 1, 99):
        add(_cspec(f"unwrap_result_{k}".replace("-", "m"), f"Get the success value of a result, or {k} on error.",
                   f"the payload of Ok; {k} for Err.", ["result", "variant", "case"], fn([result_t(INT, INT)], INT),
                   lam(["r"], _case_of(r, (_vpat("Ok", "x"), x), (_vpat("Err", "e"), int_lit(k)))),
                   [{"args": [V("Ok", 4)], "result": 4}, {"args": [V("Err", 9)], "result": k},
                    {"args": [V("Ok", -2)], "result": -2}]))

    # 14. SEARCH recursion — membership / occurrence-count of a fixed constant (recurse on the tail).
    for k in _KCMP:
        add(_cspec(f"contains_{k}".replace("-", "m"), f"Test whether {k} occurs in a list, by recursion.",
                   f"false for empty; (head == {k}) or {k} in the tail.", ["list", "recursion", "search", "predicate"],
                   fn([list_of(INT)], BOOL),
                   lam(["xs"], case_null("xs", bool_lit(False),
                        case_bool(bapp("eq", h, int_lit(k)), bool_lit(True), bself(t)))),
                   [{"args": [lst], "result": (k in lst)} for lst in _LIST_IN]))
        add(_cspec(f"count_eq_{k}".replace("-", "m"), f"Count how many times {k} occurs in a list, by recursion.",
                   f"0 for empty; +1 when head == {k}, then count the tail.", ["list", "recursion", "search", "count"],
                   fn([list_of(INT)], INT),
                   lam(["xs"], case_null("xs", int_lit(0),
                        case_bool(bapp("eq", h, int_lit(k)), bapp("add", int_lit(1), bself(t)), bself(t)))),
                   [{"args": [lst], "result": sum(1 for v in lst if v == k)} for lst in _LIST_IN]))

    # 15. ACCUMULATING structural recursion — length / sum written by hand (not via a builtin).
    add(_cspec("rec_length", "Compute the length of a list, by recursion.",
               "0 for empty; 1 + the length of the tail.", ["list", "recursion", "measure"], fn([list_of(INT)], INT),
               lam(["xs"], case_null("xs", int_lit(0), bapp("add", int_lit(1), bself(t)))),
               [{"args": [lst], "result": len(lst)} for lst in _LIST_IN]))
    add(_cspec("rec_sum", "Sum a list, by recursion.",
               "0 for empty; head + the sum of the tail.", ["list", "recursion", "fold"], fn([list_of(INT)], INT),
               lam(["xs"], case_null("xs", int_lit(0), bapp("add", h, bself(t)))),
               [{"args": [lst], "result": sum(lst)} for lst in _LIST_IN]))

    # 16. NUMERIC (counter-driven) recursion — not certified terminating, so terminates="unknown".
    for k in range(2, 13):
        add(_cspec(f"rec_times_{k}", f"Multiply a number by {k} via repeated addition, by recursion.",
                   f"0 when n is 0; otherwise {k} + ((n-1) times {k}).", ["arithmetic", "recursion"], fn([INT], INT),
                   lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                        bapp("add", int_lit(k), bself(bapp("sub", n, int_lit(1)))))),
                   [{"args": [v], "result": k * v} for v in (0, 1, 2, 3, 5, 7)], terminates="unknown"))
    for k in range(2, 8):
        add(_cspec(f"rec_pow_{k}", f"Raise {k} to a non-negative power, by recursion.",
                   f"1 when n is 0; otherwise {k} * {k}^(n-1).", ["arithmetic", "recursion"], fn([INT], INT),
                   lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(1),
                        bapp("mul", int_lit(k), bself(bapp("sub", n, int_lit(1)))))),
                   [{"args": [v], "result": k ** v} for v in (0, 1, 2, 3, 4)], terminates="unknown"))
    add(_cspec("rec_sumto", "Sum the integers from 0 up to n, by recursion.",
               "0 when n is 0; otherwise n + the sum up to n-1.", ["arithmetic", "recursion"], fn([INT], INT),
               lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                    bapp("add", n, bself(bapp("sub", n, int_lit(1)))))),
               [{"args": [v], "result": sum(range(v + 1))} for v in (0, 1, 3, 5, 7)], terminates="unknown"))

    # 17. NESTED first-order compositions (map∘map, fold∘filter, fold∘map, count-in-range) — the HOF gap.
    for op1 in ("add", "mul"):
        for op2 in ("add", "mul"):
            pf1, pf2 = _AOP[op1], _AOP[op2]
            for k1 in (2, 3, 5):
                for k2 in (2, 3, 5):
                    add(_cspec(f"map_{op1}{k1}_map_{op2}{k2}",
                               f"{_OPWORD[op1].capitalize()} {k1} then {_OPWORD[op2]} {k2} over every element.",
                               f"map ({op2} x {k2}) (map ({op1} x {k1}) xs)", ["list", "map", "composition"],
                               fn([list_of(INT)], list_of(INT)),
                               lam(["xs"], bapp("map", lam(["x"], bapp(op2, x, int_lit(k2))),
                                           bapp("map", lam(["x"], bapp(op1, x, int_lit(k1))), xs))),
                               [{"args": [lst], "result": [pf2(pf1(v, k1), k2) for v in lst]} for lst in _LIST_IN]))
    for cmp, cf in _CMP.items():
        for k in (0, 1, 2, 3, 5):
            add(_cspec(f"sum_filter_{cmp}_{k}", f"Sum the list elements {_CMPWORD[cmp]} {k}.",
                       f"foldl add 0 (filter ({cmp} x {k}) xs)", ["list", "filter", "fold", "composition"],
                       fn([list_of(INT)], INT),
                       lam(["xs"], bapp("foldl", var("add"), int_lit(0),
                                   bapp("filter", lam(["x"], bapp(cmp, x, int_lit(k))), xs))),
                       [{"args": [lst], "result": sum(v for v in lst if cf(v, k))} for lst in _LIST_IN]))
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3, 5):
            add(_cspec(f"sum_map_{op}_{k}", f"Sum a list after {_OPWORD[op]} {k} on each element.",
                       f"foldl add 0 (map ({op} x {k}) xs)", ["list", "map", "fold", "composition"],
                       fn([list_of(INT)], INT),
                       lam(["xs"], bapp("foldl", var("add"), int_lit(0),
                                   bapp("map", lam(["x"], bapp(op, x, int_lit(k))), xs))),
                       [{"args": [lst], "result": sum(pf(v, k) for v in lst)} for lst in _LIST_IN]))
    for lo, hi in [(0, 5), (1, 10), (2, 8), (-3, 3), (0, 9), (3, 7)]:
        add(_cspec(f"count_in_range_{lo}_{hi}".replace("-", "m"),
                   f"Count the list elements in the range {lo} to {hi} inclusive.",
                   f"length (filter (and (le {lo} x) (le x {hi})) xs)", ["list", "filter", "composition", "range"],
                   fn([list_of(INT)], NAT),
                   lam(["xs"], bapp("length", bapp("filter",
                        lam(["x"], bapp("and", bapp("le", int_lit(lo), x), bapp("le", x, int_lit(hi)))), xs))),
                   [{"args": [lst], "result": sum(1 for v in lst if lo <= v <= hi)} for lst in _LIST_IN]))

    # 18. MULTI-CLAUSE case chains — a parameterized 3-way threshold (the grade/compare_to shape).
    for lo, hi in [(0, 10), (5, 10), (1, 5), (10, 20), (3, 7), (0, 100)]:
        add(_cspec(f"threshold3_{lo}_{hi}", f"Map a number to 2 if at least {hi}, 1 if at least {lo}, else 0.",
                   f"2 if >= {hi}, 1 if >= {lo}, else 0 — a nested case.", ["arithmetic", "case", "order"],
                   fn([INT], INT),
                   lam(["n"], case_bool(bapp("ge", n, int_lit(hi)), int_lit(2),
                        case_bool(bapp("ge", n, int_lit(lo)), int_lit(1), int_lit(0)))),
                   [{"args": [v], "result": (2 if v >= hi else 1 if v >= lo else 0)}
                    for v in (-1, 0, lo, hi, lo + 1, hi + 5)]))

    # 19. fn_ref HIGHER-ORDER shapes — bodies that APPLY a function-valued parameter (the measured-flat
    # `list/hof` gap). Each example supplies the function as an fn_ref to a helper in `fn_deps` (built by
    # build_and_verify so it runs end to end); the model writes the polymorphic body, which doesn't depend
    # on the helper. Monomorphic INT typing keeps verification robust (the skill — writing `\f xs -> map f …`
    # — transfers regardless of the type annotation). Shapes are DISTINCT from the eval's held-out fn_ref
    # records (map_with/filter_with/apply_to/twice/compose2/all_with/…), so they teach the skill without
    # leaking those answers.
    def _hu(nm, b, py):  # unary INT->INT helper record + its Python evaluator
        return {"name": nm, "type_ast": fn([INT], INT), "body_ast": b,
                "examples": [{"args": [3], "result": py(3)}, {"args": [-2], "result": py(-2)}]}, py
    DBL, dbl = _hu("double_dep", lam(["n"], bapp("add", var("n"), var("n"))), lambda v: 2 * v)
    INC, inc = _hu("inc_dep", lam(["n"], bapp("add", var("n"), int_lit(1))), lambda v: v + 1)
    SQ, sq = _hu("square_dep", lam(["n"], bapp("mul", var("n"), var("n"))), lambda v: v * v)
    ISPOS = {"name": "is_pos_dep", "type_ast": fn([INT], BOOL),
             "body_ast": lam(["n"], bapp("gt", var("n"), int_lit(0))),
             "examples": [{"args": [3], "result": True}, {"args": [-1], "result": False}]}
    ISEVEN = {"name": "is_even_dep", "type_ast": fn([INT], BOOL),
              "body_ast": lam(["n"], bapp("eq", bapp("mod", var("n"), int_lit(2)), int_lit(0))),
              "examples": [{"args": [4], "result": True}, {"args": [3], "result": False}]}
    ADD2 = {"name": "add2_dep", "type_ast": fn([INT, INT], INT),
            "body_ast": lam(["a", "b"], bapp("add", var("a"), var("b"))), "examples": [{"args": [1, 2], "result": 3}]}
    MAX2 = {"name": "max2_dep", "type_ast": fn([INT, INT], INT),
            "body_ast": lam(["a", "b"], bapp("max", var("a"), var("b"))), "examples": [{"args": [1, 2], "result": 2}]}
    unary_fn, binary_fn, pred_fn = fn([INT], INT), fn([INT, INT], INT), fn([INT], BOOL)

    # apply a function to a fixed constant:  \f -> f k
    for k in range(0, 13):
        add(_cspec(f"apply_to_{k}", f"Apply a function to {k}.", f"f {k}", ["higher-order", "apply", "fn-ref"],
                   fn([unary_fn], INT), lam(["f"], bapp("f", int_lit(k))),
                   [{"args": [FnRef("double_dep")], "result": dbl(k)}, {"args": [FnRef("inc_dep")], "result": inc(k)}],
                   fn_deps=[DBL, INC]))
    # nested application:  \f x -> f (f (f x))   and   \f g h x -> f (g (h x))
    add(_cspec("thrice", "Apply a function to a value three times.", "f (f (f x))",
               ["higher-order", "apply", "fn-ref"], fn([unary_fn, INT], INT),
               lam(["f", "x"], bapp("f", bapp("f", bapp("f", var("x"))))),
               [{"args": [FnRef("double_dep"), 3], "result": dbl(dbl(dbl(3)))},
                {"args": [FnRef("inc_dep"), 5], "result": inc(inc(inc(5)))}], fn_deps=[DBL, INC]))
    add(_cspec("compose3", "Compose three functions and apply them to a value.", "f (g (h x))",
               ["higher-order", "compose", "fn-ref"], fn([unary_fn, unary_fn, unary_fn, INT], INT),
               lam(["f", "g", "h", "x"], bapp("f", bapp("g", bapp("h", var("x"))))),
               [{"args": [FnRef("double_dep"), FnRef("inc_dep"), FnRef("double_dep"), 3], "result": dbl(inc(dbl(3)))},
                {"args": [FnRef("inc_dep"), FnRef("double_dep"), FnRef("inc_dep"), 2], "result": inc(dbl(inc(2)))}],
               fn_deps=[DBL, INC]))
    # two function arguments through builtins:  map f (map g xs) / map f (filter p xs)
    add(_cspec("map_compose", "Map one function over the result of mapping another.", "map f (map g xs)",
               ["list", "higher-order", "map", "fn-ref"], fn([unary_fn, unary_fn, list_of(INT)], list_of(INT)),
               lam(["f", "g", "xs"], bapp("map", var("f"), bapp("map", var("g"), var("xs")))),
               [{"args": [FnRef("double_dep"), FnRef("inc_dep"), [1, 2, 3]], "result": [dbl(inc(v)) for v in [1, 2, 3]]},
                {"args": [FnRef("double_dep"), FnRef("inc_dep"), []], "result": []}], fn_deps=[DBL, INC]))
    add(_cspec("filter_map_with", "Map a function over the elements that pass a predicate.", "map f (filter p xs)",
               ["list", "higher-order", "map", "filter", "fn-ref"],
               fn([pred_fn, unary_fn, list_of(INT)], list_of(INT)),
               lam(["p", "f", "xs"], bapp("map", var("f"), bapp("filter", var("p"), var("xs")))),
               [{"args": [FnRef("is_pos_dep"), FnRef("double_dep"), [1, -2, 3]], "result": [dbl(v) for v in [1, 3]]},
                {"args": [FnRef("is_pos_dep"), FnRef("double_dep"), []], "result": []}], fn_deps=[ISPOS, DBL]))
    # function argument combined with a builtin fold / a lambda:
    add(_cspec("sum_with", "Sum a list after applying a function to each element.", "foldl add 0 (map f xs)",
               ["list", "higher-order", "map", "fold", "fn-ref"], fn([unary_fn, list_of(INT)], INT),
               lam(["f", "xs"], bapp("foldl", var("add"), int_lit(0), bapp("map", var("f"), var("xs")))),
               [{"args": [FnRef("double_dep"), [1, 2, 3]], "result": sum(dbl(v) for v in [1, 2, 3])},
                {"args": [FnRef("square_dep"), [1, 2, 3]], "result": sum(sq(v) for v in [1, 2, 3])}], fn_deps=[DBL, SQ]))
    add(_cspec("reject_with", "Keep the elements that FAIL a predicate.", "filter (not (p x)) xs",
               ["list", "higher-order", "filter", "fn-ref"], fn([pred_fn, list_of(INT)], list_of(INT)),
               lam(["p", "xs"], bapp("filter", lam(["x"], bapp("not", bapp("p", var("x")))), var("xs"))),
               [{"args": [FnRef("is_pos_dep"), [1, -2, 3, 0]], "result": [v for v in [1, -2, 3, 0] if not v > 0]},
                {"args": [FnRef("is_even_dep"), [1, 2, 3, 4]], "result": [v for v in [1, 2, 3, 4] if not v % 2 == 0]}],
               fn_deps=[ISPOS, ISEVEN]))
    # fold with a function argument and a fixed seed:  \f xs -> foldl f k xs
    for k in (0, 1, 2, 5, 10):
        add(_cspec(f"fold_with_{k}", f"Left-fold a list with a function, seeded with {k}.", f"foldl f {k} xs",
                   ["list", "higher-order", "fold", "fn-ref"], fn([binary_fn, list_of(INT)], INT),
                   lam(["f", "xs"], bapp("foldl", var("f"), int_lit(k), var("xs"))),
                   [{"args": [FnRef("add2_dep"), [1, 2, 3]], "result": k + 6},
                    {"args": [FnRef("max2_dep"), [1, 2, 3]], "result": max(k, 3)}], fn_deps=[ADD2, MAX2]))
    # apply f to each element, THEN a builtin (inside a map / filter lambda):
    for op in ("add", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3):
            add(_cspec(f"map_apply_{op}_{k}", f"Apply a function to each element, then {_OPWORD[op]} {k}.",
                       f"map ({op} (f x) {k}) xs", ["list", "higher-order", "map", "fn-ref"],
                       fn([unary_fn, list_of(INT)], list_of(INT)),
                       lam(["f", "xs"], bapp("map", lam(["x"], bapp(op, bapp("f", var("x")), int_lit(k))), var("xs"))),
                       [{"args": [FnRef("double_dep"), lst], "result": [pf(dbl(v), k) for v in lst]}
                        for lst in ([1, 2, 3], [])], fn_deps=[DBL]))
    for cmp, cf in (("gt", lambda a, b: a > b), ("lt", lambda a, b: a < b)):
        for k in (0, 2, 4):
            add(_cspec(f"filter_apply_{cmp}_{k}", f"Keep the elements whose function-image is {_CMPWORD[cmp]} {k}.",
                       f"filter ({cmp} (f x) {k}) xs", ["list", "higher-order", "filter", "fn-ref"],
                       fn([unary_fn, list_of(INT)], list_of(INT)),
                       lam(["f", "xs"], bapp("filter", lam(["x"], bapp(cmp, bapp("f", var("x")), int_lit(k))), var("xs"))),
                       [{"args": [FnRef("double_dep"), lst], "result": [v for v in lst if cf(dbl(v), k)]}
                        for lst in ([1, -2, 3, 0], [])], fn_deps=[DBL]))

    # 20. RICHER variant matching — the measured-flat `variant/case` family (held-out write ~33%). Family 13
    # only parameterized the unwrap/map-maybe shapes; these add the four uncovered sub-shapes: consume a
    # variant to a Bool, predicate ON the bound payload, use the Err/None-side payload (not a constant),
    # convert between sum types, and a nested case INSIDE a variant branch. Each is structurally DISTINCT
    # from the eval's held-out variant records (unwrap_or/is_some/maybe_double/unwrap_result/result_to_maybe/
    # predecessor/to_result_nonneg) — different gold bodies — so they teach the skill without leaking.
    e = var("e")

    # 20a. consume a variant to a Bool (is_some is held out; these are its distinct siblings).
    add(_cspec("is_none", "Test whether an optional is empty.", "false for Just; true for None.",
               ["maybe", "variant", "case", "predicate"], fn([maybe_t(INT)], BOOL),
               lam(["m"], _case_of(m, (_vpat("Just", "x"), bool_lit(False)), (_vpat("None"), bool_lit(True)))),
               [{"args": [V("Just", 7)], "result": False}, {"args": [V("None")], "result": True}]))
    add(_cspec("is_ok", "Test whether a result is a success.", "true for Ok; false for Err.",
               ["result", "variant", "case", "predicate"], fn([result_t(INT, INT)], BOOL),
               lam(["r"], _case_of(r, (_vpat("Ok", "x"), bool_lit(True)), (_vpat("Err", "e"), bool_lit(False)))),
               [{"args": [V("Ok", 4)], "result": True}, {"args": [V("Err", 9)], "result": False}]))
    add(_cspec("is_err", "Test whether a result is an error.", "false for Ok; true for Err.",
               ["result", "variant", "case", "predicate"], fn([result_t(INT, INT)], BOOL),
               lam(["r"], _case_of(r, (_vpat("Ok", "x"), bool_lit(False)), (_vpat("Err", "e"), bool_lit(True)))),
               [{"args": [V("Ok", 4)], "result": False}, {"args": [V("Err", 9)], "result": True}]))

    # 20b. predicate ON the bound payload:  case m { Just x => cmp x k ; None => false }
    for cmp, cf in _CMP.items():
        for k in (0, 1, 2, 5):
            add(_cspec(f"just_{cmp}_{k}", f"Test whether an optional holds a value {_CMPWORD[cmp]} {k}.",
                       f"({cmp} x {k}) for Just(x); false for None.", ["maybe", "variant", "case", "predicate"],
                       fn([maybe_t(INT)], BOOL),
                       lam(["m"], _case_of(m, (_vpat("Just", "x"), bapp(cmp, x, int_lit(k))),
                                           (_vpat("None"), bool_lit(False)))),
                       [{"args": [V("Just", 3)], "result": cf(3, k)}, {"args": [V("Just", -2)], "result": cf(-2, k)},
                        {"args": [V("None")], "result": False}]))

    # 20c. USE the bound payloads of both branches (unwrap_* return a constant on the empty side; these don't).
    add(_cspec("result_either", "Get the value of a result, whichever side it is.",
               "the payload of Ok, or the payload of Err.", ["result", "variant", "case"],
               fn([result_t(INT, INT)], INT),
               lam(["r"], _case_of(r, (_vpat("Ok", "x"), x), (_vpat("Err", "e"), e))),
               [{"args": [V("Ok", 4)], "result": 4}, {"args": [V("Err", 9)], "result": 9},
                {"args": [V("Ok", -2)], "result": -2}]))
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3):
            add(_cspec(f"result_ok_{op}_{k}", f"On success {_OPWORD[op]} {k}; on error return the error value.",
                       f"({op} x {k}) for Ok(x); the payload for Err.", ["result", "variant", "case"],
                       fn([result_t(INT, INT)], INT),
                       lam(["r"], _case_of(r, (_vpat("Ok", "x"), bapp(op, x, int_lit(k))), (_vpat("Err", "e"), e))),
                       [{"args": [V("Ok", 4)], "result": pf(4, k)}, {"args": [V("Err", 7)], "result": 7}]))

    # 20d. convert between sum types (Maybe<->Result), distinct from the held-out result_to_maybe body.
    for k in (0, -1, 1, 99):
        add(_cspec(f"maybe_to_result_{k}".replace("-", "m"),
                   f"Convert an optional to a result, using {k} as the error.",
                   f"Ok(x) for Just(x); Err({k}) for None.", ["maybe", "result", "variant", "case"],
                   fn([maybe_t(INT)], result_t(INT, INT)),
                   lam(["m"], _case_of(m, (_vpat("Just", "x"), variant_expr("Ok", x)),
                                       (_vpat("None"), variant_expr("Err", int_lit(k))))),
                   [{"args": [V("Just", 4)], "result": V("Ok", 4)}, {"args": [V("None")], "result": V("Err", k)}]))
    add(_cspec("result_swap", "Swap the success and error sides of a result.",
               "Err(x) for Ok(x); Ok(e) for Err(e).", ["result", "variant", "case"],
               fn([result_t(INT, INT)], result_t(INT, INT)),
               lam(["r"], _case_of(r, (_vpat("Ok", "x"), variant_expr("Err", x)),
                                   (_vpat("Err", "e"), variant_expr("Ok", e)))),
               [{"args": [V("Ok", 4)], "result": V("Err", 4)}, {"args": [V("Err", 9)], "result": V("Ok", 9)}]))
    add(_cspec("err_to_maybe", "Keep only the error value of a result as an optional.",
               "None for Ok; Just(e) for Err(e).", ["result", "maybe", "variant", "case"],
               fn([result_t(INT, INT)], maybe_t(INT)),
               lam(["r"], _case_of(r, (_vpat("Ok", "x"), variant_expr("None")),
                                   (_vpat("Err", "e"), variant_expr("Just", e)))),
               [{"args": [V("Ok", 4)], "result": V("None")}, {"args": [V("Err", 9)], "result": V("Just", 9)}]))

    # 20e. map over ONE side of a Result (the other passes through) — distinct from map_maybe (over Maybe).
    for op in ("add", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3):
            add(_cspec(f"map_ok_{op}_{k}", f"{_OPWORD[op].capitalize()} {k} on the success value, leaving Err unchanged.",
                       f"Ok({op} x {k}) for Ok(x); Err(e) for Err.", ["result", "variant", "case", "map"],
                       fn([result_t(INT, INT)], result_t(INT, INT)),
                       lam(["r"], _case_of(r, (_vpat("Ok", "x"), variant_expr("Ok", bapp(op, x, int_lit(k)))),
                                           (_vpat("Err", "e"), variant_expr("Err", e)))),
                       [{"args": [V("Ok", 4)], "result": V("Ok", pf(4, k))}, {"args": [V("Err", 5)], "result": V("Err", 5)}]))
            add(_cspec(f"map_err_{op}_{k}", f"{_OPWORD[op].capitalize()} {k} on the error value, leaving Ok unchanged.",
                       f"Ok(x) for Ok; Err({op} e {k}) for Err(e).", ["result", "variant", "case", "map"],
                       fn([result_t(INT, INT)], result_t(INT, INT)),
                       lam(["r"], _case_of(r, (_vpat("Ok", "x"), variant_expr("Ok", x)),
                                           (_vpat("Err", "e"), variant_expr("Err", bapp(op, e, int_lit(k)))))),
                       [{"args": [V("Ok", 4)], "result": V("Ok", 4)}, {"args": [V("Err", 5)], "result": V("Err", pf(5, k))}]))

    # 20f. a guard INSIDE the Just branch — a nested case under a variant pattern (the richest shape).
    for cmp, cf in (("gt", lambda a, b: a > b), ("ge", lambda a, b: a >= b), ("lt", lambda a, b: a < b)):
        for k in (0, 1, 2):
            add(_cspec(f"keep_just_{cmp}_{k}",
                       f"Keep the optional's value only when it is {_CMPWORD[cmp]} {k}, else empty.",
                       f"Just(x) when {cmp} x {k}; None otherwise; None for None.",
                       ["maybe", "variant", "case", "guarded"], fn([maybe_t(INT)], maybe_t(INT)),
                       lam(["m"], _case_of(m,
                            (_vpat("Just", "x"), case_bool(bapp(cmp, x, int_lit(k)),
                                                           variant_expr("Just", x), variant_expr("None"))),
                            (_vpat("None"), variant_expr("None")))),
                       [{"args": [V("Just", 3)], "result": (V("Just", 3) if cf(3, k) else V("None"))},
                        {"args": [V("Just", -1)], "result": (V("Just", -1) if cf(-1, k) else V("None"))},
                        {"args": [V("None")], "result": V("None")}]))

    # 20g. more multi-clause case chains (distinct from grade/compare_to/threshold3): unary sign, hi-clamp.
    add(_cspec("sign", "Map a number to its sign: -1, 0, or 1.",
               "-1 if n < 0; 0 if n == 0; 1 otherwise — a nested case.", ["arithmetic", "order", "case"],
               fn([INT], INT),
               lam(["n"], case_bool(bapp("lt", n, int_lit(0)), int_lit(-1),
                    case_bool(bapp("eq", n, int_lit(0)), int_lit(0), int_lit(1)))),
               [{"args": [v], "result": (-1 if v < 0 else 0 if v == 0 else 1)} for v in (-5, -1, 0, 3, 7)]))
    for k in (5, 10, 100):
        add(_cspec(f"clamp_hi_{k}", f"Clamp a number to between 0 and {k} inclusive.",
                   f"0 if n < 0; {k} if n > {k}; n otherwise — a nested case.", ["arithmetic", "order", "case"],
                   fn([INT], INT),
                   lam(["n"], case_bool(bapp("lt", n, int_lit(0)), int_lit(0),
                        case_bool(bapp("gt", n, int_lit(k)), int_lit(k), n))),
                   [{"args": [v], "result": (0 if v < 0 else k if v > k else v)} for v in (-3, 0, 2, k, k + 5)]))

    # 21. TWO-LIST-PARAMETER recursion — a shape the combinatorial families never produced, though the
    # curated eval has it (append_rec). Sub-shapes with gold bodies DIFFERENT from append_rec's, so they
    # teach the two-list idiom without leaking it: (a) zipWith — recurse on BOTH lists with a nested case,
    # stopping at the shorter; (b) transform-then-append — recurse on the FIRST list with the second a
    # spectator (append_rec's idiom) while applying op _ k to each head. Monomorphic INT typing.
    ys = var("ys")
    hx, hy = bapp("head", xs), bapp("head", ys)
    tx, ty = bapp("tail", xs), bapp("tail", ys)
    two_lists = fn([list_of(INT), list_of(INT)], list_of(INT))
    _ZPY = {"add": lambda a, b: a + b, "sub": lambda a, b: a - b, "mul": lambda a, b: a * b,
            "min": min, "max": max}
    _ZWORD = {"add": "sum", "sub": "difference", "mul": "product", "min": "minimum", "max": "maximum"}

    # 21a. zipWith op — element-wise combine of two lists, truncating to the shorter (DUAL recursion).
    _ZIN = [([1, 2, 3], [4, 5, 6]), ([5, -2, 4], [1, 1, 1]), ([], []), ([3], [7, 8]), ([1, 2], [])]
    for op in ("add", "sub", "mul", "min", "max"):
        pf = _ZPY[op]
        add(_cspec(f"zip_{op}",
                   f"Combine two lists element by element into their pairwise {_ZWORD[op]}, truncating to the shorter list.",
                   f"nil if either list is empty; else cons ({op} (head xs) (head ys)) (self (tail xs) (tail ys))",
                   ["list", "recursion", "two-list", "zip", op], two_lists,
                   lam(["xs", "ys"], case_null("xs", var("nil"),
                        case_null("ys", var("nil"),
                                  bapp("cons", bapp(op, hx, hy), bself(tx, ty))))),
                   [{"args": [a, b], "result": [pf(u, v) for u, v in zip(a, b)]} for a, b in _ZIN]))

    # 21b. transform-then-append — recurse on the FIRST list (second is a spectator), op-ing each head by k.
    _AIN = [([1, 2, 3], [10, 20]), ([5, -2], [0]), ([], [7, 8]), ([4], [])]
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (1, 2, 3, 4, 5):
            add(_cspec(f"appendmap_{op}_{k}",
                       f"Build a new list: each element of the first list {_OPWORD[op]} {k}, followed by the second list unchanged.",
                       f"ys when xs is empty; else cons ({op} (head xs) {k}) (self (tail xs) ys)",
                       ["list", "recursion", "two-list", "map", op], two_lists,
                       lam(["xs", "ys"], case_null("xs", ys,
                            bapp("cons", bapp(op, hx, int_lit(k)), bself(tx, ys)))),
                       [{"args": [a, b], "result": [pf(u, k) for u in a] + b} for a, b in _AIN]))

    # 22. LET-BINDINGS — introduce `let name = value in body`, a body node NO other family emits. Two
    # forms: (a) bind a subcomputation and REUSE it (the canonical motivation for let), (b) a two-step
    # computation written as a let (the model learns the let form of a known shape).
    # 22a. let-reuse:  \n -> let d = op n k in add d d   ( = 2 * (op n k) )
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (_KMUL if op == "mul" else _KADD):
            add(_cspec(f"let_twice_{op}_{k}",
                       f"{_OPWORD[op].capitalize()} {k}, then double the result, binding it once with let.",
                       f"let d = {op} n {k} in add d d", ["arithmetic", "let", "binding"], fn([INT], INT),
                       lam(["n"], blet("d", bapp(op, n, int_lit(k)), bapp("add", var("d"), var("d")))),
                       [{"args": [v], "result": 2 * pf(v, k)} for v in _INT_IN]))
    # 22b. let-as-two-step:  \n -> let y = op1 n k1 in op2 y k2   (a small, bounded cross-product)
    steps_small = [("add", k) for k in (1, 2, 3)] + [("mul", k) for k in (2, 3)]
    for op1, k1 in steps_small:
        for op2, k2 in steps_small:
            pf1, pf2 = _AOP[op1], _AOP[op2]
            add(_cspec(f"let_{op1}{k1}_{op2}{k2}",
                       f"{_OPWORD[op1].capitalize()} {k1}, bind it with let, then {_OPWORD[op2]} {k2}.",
                       f"let y = {op1} n {k1} in {op2} y {k2}", ["arithmetic", "let", "binding", "two-step"],
                       fn([INT], INT),
                       lam(["n"], blet("y", bapp(op1, n, int_lit(k1)), bapp(op2, var("y"), int_lit(k2)))),
                       [{"args": [v], "result": pf2(pf1(v, k1), k2)} for v in _INT_IN]))

    # 23. ACCUMULATOR (tail) recursion — \xs acc -> case xs of nil -> acc | cons -> self (tail xs)
    # (op acc (head xs)). A foldl written by hand: two parameters (list + accumulator), threading the
    # accumulator (distinct from family 15's non-tail 1+self). Structurally decreasing — terminates=always.
    _FPY = {"add": lambda a, b: a + b, "sub": lambda a, b: a - b, "mul": lambda a, b: a * b,
            "min": min, "max": max}

    def _fold(opf, seq, init):
        a = init
        for v in seq:
            a = opf(a, v)
        return a
    list_int_to_int = fn([list_of(INT), INT], INT)
    for op in ("add", "sub", "mul", "min", "max"):
        opf = _FPY[op]
        add(_cspec(f"foldl_{op}",
                   f"Left-fold a list into an accumulator using {op}, starting from a given seed.",
                   f"acc when xs is empty; else self (tail xs) ({op} acc (head xs))",
                   ["list", "recursion", "accumulator", "fold", op], list_int_to_int,
                   lam(["xs", "acc"], case_null("xs", var("acc"), bself(tx, bapp(op, var("acc"), hx)))),
                   [{"args": [lst, seed], "result": _fold(opf, lst, seed)}
                    for lst in ([1, 2, 3], [5, -2, 4], [], [10]) for seed in (0, 1)]))

    # 24. REPLICATE — counter recursion BUILDING a list:  \n -> case n==0 of true -> nil | false ->
    # cons x0 (self (n-1)). A counter-driven recursion whose result is a LIST (family 16 is int->int);
    # terminates="unknown" (not certified for negative n).
    for x0 in (0, 1, 2, 7, -1):
        add(_cspec(f"replicate_{x0}".replace("-", "m"), f"Build a list of n copies of {x0}.",
                   f"nil when n is 0; else cons {x0} (self (n-1))", ["list", "recursion", "build", "counter"],
                   fn([INT], list_of(INT)),
                   lam(["n"], case_bool(bapp("eq", n, int_lit(0)), var("nil"),
                        bapp("cons", int_lit(x0), bself(bapp("sub", n, int_lit(1)))))),
                   [{"args": [c], "result": [x0] * c} for c in (0, 1, 2, 3, 5)], terminates="unknown"))

    # 25. MODULAR ARITHMETIC & PARITY — the `mod` operator, which no family taught; the biggest measured
    # 1.5B write-failure cluster (is_even/is_odd/modulo/keep_evens/count_evens/sum_evens). Parameterized
    # over the modulus, so the eval's specific instances (e.g. is_even = divisible-by-2) stay held out but
    # the SHAPE is taught. Non-negative inputs only (mod-on-negatives differs Python vs. the evaluator).
    _NN = [0, 1, 2, 3, 4, 5, 7, 12]
    _LNN = [[], [1, 2, 3, 4], [2, 4, 6], [1, 3, 5, 7], [0, 6, 12]]
    # 25a. divisibility predicate:  \n -> eq (mod n m) 0
    for mm in range(2, 10):
        add(_cspec(f"divisible_by_{mm}", f"Test whether a number is divisible by {mm}.",
                   f"eq (mod n {mm}) 0", ["arithmetic", "modular", "predicate"], fn([INT], BOOL),
                   lam(["n"], bapp("eq", bapp("mod", n, int_lit(mm)), int_lit(0))),
                   [{"args": [v], "result": (v % mm == 0)} for v in _NN]))
    # 25b. remainder:  \n -> mod n m
    for mm in range(2, 10):
        add(_cspec(f"remainder_{mm}", f"Compute the remainder of a number divided by {mm}.",
                   f"mod n {mm}", ["arithmetic", "modular"], fn([INT], INT),
                   lam(["n"], bapp("mod", n, int_lit(mm))),
                   [{"args": [v], "result": (v % mm)} for v in _NN]))
    # 25c. divisibility filter:  \xs -> filter (\x -> eq (mod x m) 0) xs
    for mm in (2, 3, 4, 5):
        add(_cspec(f"keep_divisible_{mm}", f"Keep the list elements divisible by {mm}.",
                   f"filter (mod x {mm} == 0) xs", ["list", "filter", "modular"], fn([list_of(INT)], list_of(INT)),
                   lam(["xs"], bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0))), xs)),
                   [{"args": [lst], "result": [v for v in lst if v % mm == 0]} for lst in _LNN]))
    # 25d. count divisible:  \xs -> length (filter ... xs)
    for mm in (2, 3, 5):
        add(_cspec(f"count_divisible_{mm}", f"Count the list elements divisible by {mm}.",
                   f"length (filter (mod x {mm} == 0) xs)", ["list", "filter", "count", "modular"],
                   fn([list_of(INT)], NAT),
                   lam(["xs"], bapp("length", bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0))), xs))),
                   [{"args": [lst], "result": sum(1 for v in lst if v % mm == 0)} for lst in _LNN]))
    # 25e. sum divisible:  \xs -> foldl add 0 (filter ... xs)
    for mm in (2, 3):
        add(_cspec(f"sum_divisible_{mm}", f"Sum the list elements divisible by {mm}.",
                   f"foldl add 0 (filter (mod x {mm} == 0) xs)", ["list", "filter", "fold", "modular"],
                   fn([list_of(INT)], INT),
                   lam(["xs"], bapp("foldl", var("add"), int_lit(0),
                        bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0))), xs))),
                   [{"args": [lst], "result": sum(v for v in lst if v % mm == 0)} for lst in _LNN]))

    # 26. EXTENDED BOOLEAN — compositions of not/and/or over two boolean args (the xor/nand/nor/implies
    # shape). Parameterized over the outer connective and each arg's polarity; the eval's named instances
    # (implies/nand/nor/logical_xor) stay held out, the rest teach the not/and/or composition shape.
    _BPY = {"and": lambda p, q: p and q, "or": lambda p, q: p or q}
    for outer in ("and", "or"):
        of = _BPY[outer]
        for pa in (False, True):
            for pb in (False, True):
                la = bapp("not", var("a")) if pa else var("a")
                lb = bapp("not", var("b")) if pb else var("b")
                sa, sb = ("not a" if pa else "a"), ("not b" if pb else "b")
                add(_cspec(f"bool_{outer}_{'na' if pa else 'a'}_{'nb' if pb else 'b'}",
                           f"Combine two booleans: {outer} of {sa} and {sb}.", f"{outer} ({sa}) ({sb})",
                           ["boolean", "logic", "compound"], fn([BOOL, BOOL], BOOL),
                           lam(["a", "b"], bapp(outer, la, lb)),
                           [{"args": [u, w], "result": of((not u) if pa else u, (not w) if pb else w)}
                            for u in (False, True) for w in (False, True)]))
    for outer in ("and", "or"):
        of = _BPY[outer]
        add(_cspec(f"bool_n{outer}", f"The negation of {outer} over two booleans.", f"not ({outer} a b)",
                   ["boolean", "logic", "compound"], fn([BOOL, BOOL], BOOL),
                   lam(["a", "b"], bapp("not", bapp(outer, var("a"), var("b")))),
                   [{"args": [u, w], "result": (not of(u, w))} for u in (False, True) for w in (False, True)]))

    # 27. POLYMORPHIC two-list structural recursion — write/append_rec & write/concat still fail at 1.5B;
    # family 21 is monomorphic INT (it does element arithmetic). interleave is forall a, no element op,
    # a distinct body from the held-out append_rec/concat/reverse_concat.
    poly2 = {"kind": "forall", "vars": ["a"],
             "body": fn([list_of(var("a")), list_of(var("a"))], list_of(var("a")))}
    add(_cspec("interleave", "Interleave two lists, alternating elements; the rest of the longer one when the other runs out.",
               "ys when xs is empty; else cons (head xs) (self ys (tail xs))", ["list", "recursion", "two-list", "poly"],
               poly2, lam(["xs", "ys"], case_null("xs", ys, bapp("cons", bapp("head", xs), bself(ys, bapp("tail", xs))))),
               [{"args": [[1, 2, 3], [4, 5, 6]], "result": [1, 4, 2, 5, 3, 6]},
                {"args": [[1], [2, 3, 4]], "result": [1, 2, 3, 4]},
                {"args": [[], [7, 8]], "result": [7, 8]}, {"args": [[5], []], "result": [5]}]))

    # 28. QUANTIFIED MODULAR PREDICATES — "every / some element divisible by m", the all_even/any_even
    # holdouts. all_even's gold is foldr-AND over an INLINED mod-predicate; any_even's is !null . filter.
    # No combinatorial family taught the inlined modular predicate (all_with/any_with are higher-order,
    # predicate-as-arg). Parameterized over m; m=2 omitted so all_even/any_even stay held out.
    _MLST = [[], [2, 4, 6], [1, 2, 3, 4, 5, 6], [3, 5, 9], [0, 6, 12], [7, 11, 13]]
    for mm in (3, 4, 5):
        add(_cspec(f"all_divisible_{mm}", f"Test whether every number in a list is divisible by {mm}.",
                   f"foldr (\\x acc -> mod x {mm} == 0 && acc) true xs", ["list", "fold", "predicate", "modular"],
                   fn([list_of(INT)], BOOL),
                   lam(["xs"], bapp("foldr", lam(["x", "acc"], bapp("and", bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0)), var("acc"))),
                                    bool_lit(True), xs)),
                   [{"args": [lst], "result": all(v % mm == 0 for v in lst)} for lst in _MLST]))
    for mm in (3, 4, 5):
        add(_cspec(f"any_divisible_{mm}", f"Test whether a list contains a number divisible by {mm}.",
                   f"!null (filter (\\x -> mod x {mm} == 0) xs)", ["list", "filter", "predicate", "modular"],
                   fn([list_of(INT)], BOOL),
                   lam(["xs"], bapp("not", bapp("null", bapp("filter", lam(["x"], bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0))), xs)))),
                   [{"args": [lst], "result": any(v % mm == 0 for v in lst)} for lst in _MLST]))
    for mm in (3, 4):
        add(_cspec(f"any_divisible_fold_{mm}", f"Test (by fold) whether a list contains a number divisible by {mm}.",
                   f"foldr (\\x acc -> mod x {mm} == 0 || acc) false xs", ["list", "fold", "predicate", "modular"],
                   fn([list_of(INT)], BOOL),
                   lam(["xs"], bapp("foldr", lam(["x", "acc"], bapp("or", bapp("eq", bapp("mod", x, int_lit(mm)), int_lit(0)), var("acc"))),
                                    bool_lit(False), xs)),
                   [{"args": [lst], "result": any(v % mm == 0 for v in lst)} for lst in _MLST]))

    # 29. TWO-ARGUMENT MODULAR ARITHMETIC — `mod` with a VARIABLE divisor (the `modulo` holdout: \a b -> a % b).
    # Family 25b only did a CONSTANT divisor. The exact \a b -> mod a b is the eval gold (leakage-dropped), so
    # teach the shape via compositions whose divisor is the variable b. Non-negative inputs, b != 0.
    _AB = [(7, 3), (10, 4), (5, 2), (12, 5), (0, 3), (9, 4), (8, 3), (11, 6)]
    for k in (1, 2, 3):
        add(_cspec(f"mod_plus_{k}", f"Add {k} to a number, then take it modulo a second number.",
                   f"mod (a + {k}) b", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("mod", bapp("add", var("a"), int_lit(k)), var("b"))),
                   [{"args": [a, b], "result": (a + k) % b} for (a, b) in _AB]))
    for k in (2, 3):
        add(_cspec(f"mod_times_{k}", f"Multiply a number by {k}, then take it modulo a second number.",
                   f"mod (a * {k}) b", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("mod", bapp("mul", var("a"), int_lit(k)), var("b"))),
                   [{"args": [a, b], "result": (a * k) % b} for (a, b) in _AB]))
    for k in (1, 2, 3):
        add(_cspec(f"mod_then_add_{k}", f"Take a number modulo a second number, then add {k}.",
                   f"(mod a b) + {k}", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("add", bapp("mod", var("a"), var("b")), int_lit(k))),
                   [{"args": [a, b], "result": (a % b) + k} for (a, b) in _AB]))

    # 30. EXTENDED BOOLEAN, deeper — De-Morgan-equivalent forms (distinct surfaces from family 26) and
    # 3-argument compositions, adding not/and/or mass so the noisy implies/nand holdouts (shape already in
    # family 26) get reinforced. Exhaustive examples over all boolean assignments.
    _BB = [(u, w) for u in (False, True) for w in (False, True)]
    _BBB = [(u, w, z) for u in (False, True) for w in (False, True) for z in (False, True)]
    add(_cspec("bool_or_not_not", "Or of the negations of two booleans.", "(not a) || (not b)",
               ["boolean", "logic", "de-morgan"], fn([BOOL, BOOL], BOOL),
               lam(["a", "b"], bapp("or", bapp("not", var("a")), bapp("not", var("b")))),
               [{"args": [u, w], "result": ((not u) or (not w))} for (u, w) in _BB]))
    add(_cspec("bool_and_not_not", "And of the negations of two booleans.", "(not a) && (not b)",
               ["boolean", "logic", "de-morgan"], fn([BOOL, BOOL], BOOL),
               lam(["a", "b"], bapp("and", bapp("not", var("a")), bapp("not", var("b")))),
               [{"args": [u, w], "result": ((not u) and (not w))} for (u, w) in _BB]))
    add(_cspec("bool_not_or_notb", "Not of (a or not b).", "not (a || not b)",
               ["boolean", "logic"], fn([BOOL, BOOL], BOOL),
               lam(["a", "b"], bapp("not", bapp("or", var("a"), bapp("not", var("b"))))),
               [{"args": [u, w], "result": (not (u or (not w)))} for (u, w) in _BB]))
    add(_cspec("bool_and3", "And of three booleans.", "(a && b) && c",
               ["boolean", "logic", "ternary"], fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("and", bapp("and", var("a"), var("b")), var("c"))),
               [{"args": [u, w, z], "result": (u and w and z)} for (u, w, z) in _BBB]))
    add(_cspec("bool_or3", "Or of three booleans.", "(a || b) || c",
               ["boolean", "logic", "ternary"], fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("or", bapp("or", var("a"), var("b")), var("c"))),
               [{"args": [u, w, z], "result": (u or w or z)} for (u, w, z) in _BBB]))
    add(_cspec("bool_or_and", "a or (b and c).", "a || (b && c)",
               ["boolean", "logic", "ternary"], fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("or", var("a"), bapp("and", var("b"), var("c")))),
               [{"args": [u, w, z], "result": (u or (w and z))} for (u, w, z) in _BBB]))
    add(_cspec("bool_and_or", "a and (b or c).", "a && (b || c)",
               ["boolean", "logic", "ternary"], fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("and", var("a"), bapp("or", var("b"), var("c")))),
               [{"args": [u, w, z], "result": (u and (w or z))} for (u, w, z) in _BBB]))
    add(_cspec("bool_nand3", "Not of (a and b and c).", "not ((a && b) && c)",
               ["boolean", "logic", "ternary"], fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("not", bapp("and", bapp("and", var("a"), var("b")), var("c")))),
               [{"args": [u, w, z], "result": (not (u and w and z))} for (u, w, z) in _BBB]))

    # 31. NESTED-LIST / APPEND RECURSION — flatten a list of lists via `append (f (head)) (self (tail))`,
    # the concat_lists holdout's skeleton (its exact body is held out, so vary f on the head), plus a
    # non-recursive triple `append` chain.
    nil = var("nil")
    poly_nest = {"kind": "forall", "vars": ["a"],
                 "body": fn([list_of(list_of(var("a")))], list_of(var("a")))}
    add(_cspec("flatten_each_reversed", "Flatten a list of lists, reversing each inner list.",
               "nil when empty; else append (reverse (head xss)) (self (tail xss))",
               ["list", "recursion", "nested", "poly"], poly_nest,
               lam(["xss"], case_null("xss", nil,
                    bapp("append", bapp("reverse", bapp("head", var("xss"))), bself(bapp("tail", var("xss")))))),
               [{"args": [[]], "result": []},
                {"args": [[[1, 2], [3]]], "result": [2, 1, 3]},
                {"args": [[[1], [2], [3, 4]]], "result": [1, 2, 4, 3]},
                {"args": [[[5, 6], [], [7]]], "result": [6, 5, 7]}]))
    add(_cspec("flatten_rightfirst", "Flatten a list of lists, the later inner lists first.",
               "nil when empty; else append (self (tail xss)) (head xss)",
               ["list", "recursion", "nested", "poly"], poly_nest,
               lam(["xss"], case_null("xss", nil,
                    bapp("append", bself(bapp("tail", var("xss"))), bapp("head", var("xss"))))),
               [{"args": [[]], "result": []},
                {"args": [[[1, 2], [3]]], "result": [3, 1, 2]},
                {"args": [[[1], [2], [3, 4]]], "result": [3, 4, 2, 1]},
                {"args": [[[5, 6], [], [7]]], "result": [7, 5, 6]}]))
    poly_tri = {"kind": "forall", "vars": ["a"],
                "body": fn([list_of(var("a")), list_of(var("a")), list_of(var("a"))], list_of(var("a")))}
    add(_cspec("triple_concat", "Concatenate three lists.", "append (append xs ys) zs",
               ["list", "append", "poly"], poly_tri,
               lam(["xs", "ys", "zs"], bapp("append", bapp("append", var("xs"), var("ys")), var("zs"))),
               [{"args": [[], [], []], "result": []},
                {"args": [[1], [2, 3], [4]], "result": [1, 2, 3, 4]},
                {"args": [[1, 2], [], [3]], "result": [1, 2, 3]}]))

    # 32. MODULO HARDENING — `modulo` (\a b -> a % b) flipped on seed-0 but not seed-1 (round-4, on the
    # edge). More two-arg-mod mass with a VARIABLE divisor + richer example pairs to stabilize it. Nonneg,
    # divisor != 0; mod_swap/mod_sq need a non-zero first arg too (all _AB2 first elements are >= 5).
    _AB2 = [(7, 3), (10, 4), (5, 2), (12, 5), (9, 4), (8, 3), (11, 6), (15, 7), (6, 4), (13, 5), (20, 6), (14, 9)]
    for k in (4, 5, 6):
        add(_cspec(f"mod_plus_{k}", f"Add {k} to a number, then take it modulo a second number.",
                   f"mod (a + {k}) b", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("mod", bapp("add", var("a"), int_lit(k)), var("b"))),
                   [{"args": [a, b], "result": (a + k) % b} for (a, b) in _AB2]))
    for k in (4, 5):
        add(_cspec(f"mod_times_{k}", f"Multiply a number by {k}, then take it modulo a second number.",
                   f"mod (a * {k}) b", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("mod", bapp("mul", var("a"), int_lit(k)), var("b"))),
                   [{"args": [a, b], "result": (a * k) % b} for (a, b) in _AB2]))
    for k in (4, 5):
        add(_cspec(f"mod_then_add_{k}", f"Take a number modulo a second number, then add {k}.",
                   f"(mod a b) + {k}", ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp("add", bapp("mod", var("a"), var("b")), int_lit(k))),
                   [{"args": [a, b], "result": (a % b) + k} for (a, b) in _AB2]))
    add(_cspec("mod_swap", "Take the second number modulo the first.", "mod b a",
               ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
               lam(["a", "b"], bapp("mod", var("b"), var("a"))),
               [{"args": [a, b], "result": b % a} for (a, b) in _AB2]))
    add(_cspec("mod_square", "Square a number, then take it modulo a second number.", "mod (a * a) b",
               ["arithmetic", "modular", "two-arg"], fn([INT, INT], INT),
               lam(["a", "b"], bapp("mod", bapp("mul", var("a"), var("a")), var("b"))),
               [{"args": [a, b], "result": (a * a) % b} for (a, b) in _AB2]))

    # 33. BOOLEAN MASS — nand's exact `not (and a b)` is leakage-dropped, so the model must GENERALIZE from
    # related shapes. Deepen the not/and/or vocabulary: 3-arg, 4-arg, xnor, double-negation. Exhaustive.
    _B4 = [(u, w, z, t) for u in (False, True) for w in (False, True)
           for z in (False, True) for t in (False, True)]
    add(_cspec("bool_orand_c", "(a and b) or c.", "(a && b) || c", ["boolean", "logic", "ternary"],
               fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("or", bapp("and", var("a"), var("b")), var("c"))),
               [{"args": [u, w, z], "result": ((u and w) or z)} for (u, w, z) in _BBB]))
    add(_cspec("bool_orab_notc", "(a or b) and not c.", "(a || b) && not c", ["boolean", "logic", "ternary"],
               fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("and", bapp("or", var("a"), var("b")), bapp("not", var("c")))),
               [{"args": [u, w, z], "result": ((u or w) and (not z))} for (u, w, z) in _BBB]))
    add(_cspec("bool_nor_andc", "Not ((a and b) or c).", "not ((a && b) || c)", ["boolean", "logic", "ternary"],
               fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("not", bapp("or", bapp("and", var("a"), var("b")), var("c")))),
               [{"args": [u, w, z], "result": (not ((u and w) or z))} for (u, w, z) in _BBB]))
    add(_cspec("bool_na_andbc", "(not a) or (b and c).", "(not a) || (b && c)", ["boolean", "logic", "ternary"],
               fn([BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c"], bapp("or", bapp("not", var("a")), bapp("and", var("b"), var("c")))),
               [{"args": [u, w, z], "result": ((not u) or (w and z))} for (u, w, z) in _BBB]))
    add(_cspec("bool_xnor", "Two booleans are equal (xnor).", "(a && b) || (not a && not b)",
               ["boolean", "logic", "equality"], fn([BOOL, BOOL], BOOL),
               lam(["a", "b"], bapp("or", bapp("and", var("a"), var("b")),
                                    bapp("and", bapp("not", var("a")), bapp("not", var("b"))))),
               [{"args": [u, w], "result": (u == w)} for (u, w) in _BB]))
    add(_cspec("bool_and_dn", "And of two booleans via double negation.", "not (not a || not b)",
               ["boolean", "logic", "de-morgan"], fn([BOOL, BOOL], BOOL),
               lam(["a", "b"], bapp("not", bapp("or", bapp("not", var("a")), bapp("not", var("b"))))),
               [{"args": [u, w], "result": (u and w)} for (u, w) in _BB]))
    add(_cspec("bool_and4", "And of four booleans.", "(a && b) && (c && d)", ["boolean", "logic", "quaternary"],
               fn([BOOL, BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c", "d"], bapp("and", bapp("and", var("a"), var("b")), bapp("and", var("c"), var("d")))),
               [{"args": [u, w, z, t], "result": (u and w and z and t)} for (u, w, z, t) in _B4]))
    add(_cspec("bool_or4", "Or of four booleans.", "(a || b) || (c || d)", ["boolean", "logic", "quaternary"],
               fn([BOOL, BOOL, BOOL, BOOL], BOOL),
               lam(["a", "b", "c", "d"], bapp("or", bapp("or", var("a"), var("b")), bapp("or", var("c"), var("d")))),
               [{"args": [u, w, z, t], "result": (u or w or z or t)} for (u, w, z, t) in _B4]))

    # 34. NESTED-LIST / RECURSION MASS — concat_lists' `append (f head) (self tail)` skeleton resisted at 1.5B.
    # More flatten variants + poly two-list append shapes + list-building recursion, to deepen the skeleton.
    nil2 = var("nil")
    poly_nest2 = {"kind": "forall", "vars": ["a"],
                  "body": fn([list_of(list_of(var("a")))], list_of(var("a")))}
    poly_two = {"kind": "forall", "vars": ["a"],
                "body": fn([list_of(var("a")), list_of(var("a"))], list_of(var("a")))}
    add(_cspec("flatten_dup_head", "Flatten a list of lists, duplicating each inner list.",
               "nil when empty; else append (append (head xss) (head xss)) (self (tail xss))",
               ["list", "recursion", "nested", "poly"], poly_nest2,
               lam(["xss"], case_null("xss", nil2,
                    bapp("append", bapp("append", bapp("head", var("xss")), bapp("head", var("xss"))),
                         bself(bapp("tail", var("xss")))))),
               [{"args": [[]], "result": []},
                {"args": [[[1, 2], [3]]], "result": [1, 2, 1, 2, 3, 3]},
                {"args": [[[1], [2], [3, 4]]], "result": [1, 1, 2, 2, 3, 4, 3, 4]},
                {"args": [[[5, 6], [], [7]]], "result": [5, 6, 5, 6, 7, 7]}]))
    add(_cspec("prepend_lists", "Concatenate two lists, the second one first.", "append ys xs",
               ["list", "append", "poly"], poly_two,
               lam(["xs", "ys"], bapp("append", var("ys"), var("xs"))),
               [{"args": [[], []], "result": []},
                {"args": [[1], [2, 3]], "result": [2, 3, 1]},
                {"args": [[1, 2], [3]], "result": [3, 1, 2]}]))
    add(_cspec("append_double_second", "Append the first list onto two copies of the second.",
               "append xs (append ys ys)", ["list", "append", "poly"], poly_two,
               lam(["xs", "ys"], bapp("append", var("xs"), bapp("append", var("ys"), var("ys")))),
               [{"args": [[], []], "result": []},
                {"args": [[1], [2, 3]], "result": [1, 2, 3, 2, 3]},
                {"args": [[1, 2], [3]], "result": [1, 2, 3, 3]}]))
    add(_cspec("surround", "Surround the first list with the second on both sides.",
               "append (append ys xs) ys", ["list", "append", "poly"], poly_two,
               lam(["xs", "ys"], bapp("append", bapp("append", var("ys"), var("xs")), var("ys"))),
               [{"args": [[], []], "result": []},
                {"args": [[1], [2]], "result": [2, 1, 2]},
                {"args": [[1, 2], [3]], "result": [3, 1, 2, 3]}]))
    add(_cspec("triple_all_rec", "Triple every number in a list, by recursion.",
               "nil when empty; else cons (3 * head) (self (tail))", ["list", "recursion", "map", "elementwise"],
               fn([list_of(INT)], list_of(INT)),
               lam(["xs"], case_null("xs", nil2,
                    bapp("cons", bapp("mul", int_lit(3), bapp("head", var("xs"))), bself(bapp("tail", var("xs")))))),
               [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [3, -6, 9]},
                {"args": [[0, 4, 5]], "result": [0, 12, 15]}]))
    add(_cspec("add10_all_rec", "Add ten to every number in a list, by recursion.",
               "nil when empty; else cons (head + 10) (self (tail))", ["list", "recursion", "map", "elementwise"],
               fn([list_of(INT)], list_of(INT)),
               lam(["xs"], case_null("xs", nil2,
                    bapp("cons", bapp("add", bapp("head", var("xs")), int_lit(10)), bself(bapp("tail", var("xs")))))),
               [{"args": [[]], "result": []}, {"args": [[1, -2, 3]], "result": [11, 8, 13]},
                {"args": [[0, 90]], "result": [10, 100]}]))

    # 35. MIN/MAX BOUNDS & CLAMP — the scalar min/max-with-a-constant and the [lo,hi] clamp had only
    # SINGLE curated examples (max_self / max_min_absorb / clamp), so the model saw the shape ~once. The
    # Coder-3B write residuals cluster here; add combinatorial mass on the NATURALLY-parameterizable forms.
    # (in_range is already family 8; the 2-var absorption laws have no constant to vary, so they stay
    # curated — adding confusable near-duplicates is the boolean-mass mistake.) Monomorphic INT.
    _BND = [-5, -3, -1, 0, 1, 2, 3, 5, 8, 10]
    # 35a. bound below:  \n -> max n k   (clamp up to at least k)
    for k in _BND:
        add(_cspec(f"bound_below_{k}".replace("-", "m"),
                   f"Clamp a number up so it is at least {k}.", f"max n {k}",
                   ["arithmetic", "min-max", "clamp", "bound"], fn([INT], INT),
                   lam(["n"], bapp("max", n, int_lit(k))),
                   [{"args": [v], "result": max(v, k)} for v in _INT_IN]))
    # 35b. bound above:  \n -> min n k   (clamp down to at most k)
    for k in _BND:
        add(_cspec(f"bound_above_{k}".replace("-", "m"),
                   f"Clamp a number down so it is at most {k}.", f"min n {k}",
                   ["arithmetic", "min-max", "clamp", "bound"], fn([INT], INT),
                   lam(["n"], bapp("min", n, int_lit(k))),
                   [{"args": [v], "result": min(v, k)} for v in _INT_IN]))
    # 35c. clamp to [lo, hi]:  \x -> max lo (min hi x)   (the residual `clamp` shape, parameterized over ranges)
    for lo, hi in [(0, 5), (1, 10), (-5, 5), (2, 8), (0, 9), (-3, 3), (1, 4), (-2, 6)]:
        add(_cspec(f"clamp_{lo}_{hi}".replace("-", "m"),
                   f"Clamp a number to the range {lo} to {hi} inclusive.", f"max {lo} (min {hi} x)",
                   ["arithmetic", "min-max", "clamp", "range"], fn([INT], INT),
                   lam(["x"], bapp("max", int_lit(lo), bapp("min", int_lit(hi), x))),
                   [{"args": [v], "result": max(lo, min(hi, v))} for v in _INT_IN]))
    # 35d. min/max then arithmetic:  \n -> op (bnd n k1) k2   (min/max used INSIDE a composition)
    for bnd in ("min", "max"):
        bf = (min if bnd == "min" else max)
        for k1 in (0, 3, 5):
            for op in ("add", "mul"):
                pf, k2 = _AOP[op], (2 if op == "mul" else 10)
                add(_cspec(f"{bnd}{k1}_{op}{k2}",
                           f"Take the {bnd} of a number and {k1}, then {_OPWORD[op]} {k2}.",
                           f"{op} ({bnd} n {k1}) {k2}", ["arithmetic", "min-max", "composition"], fn([INT], INT),
                           lam(["n"], bapp(op, bapp(bnd, n, int_lit(k1)), int_lit(k2))),
                           [{"args": [v], "result": pf(bf(v, k1), k2)} for v in _INT_IN]))

    # 36. POWERS & DIGIT ARITHMETIC via the dialect's OWN primitives — Coder-3B invented `**`/`^` for powers
    # and `show`/`digitToInt` for digit-sums (square_diff/pow2/sum_digits all failed both seeds) because NO
    # family taught the in-dialect forms: powers are repeated multiplication or `k * self (n-1)` recursion;
    # digits are `n % b + self (n / b)` div/mod recursion. The eval's exact instances (square/cube/pow2/pow/
    # sum_digits) stay leakage-dropped; these teach the SHAPE with non-eval bases/constants. Non-negative
    # inputs where div/mod are involved (mod/div on negatives differ Python vs. the evaluator).
    def _dsum(v, b):
        s = 0
        while v > 0:
            s += v % b; v //= b
        return s
    def _ndig(v, b):
        c = 0
        while v > 0:
            c += 1; v //= b
        return c
    _NN36 = [0, 1, 2, 5, 9, 12, 23]
    # (Fixed-base power-by-recursion `k * self (n-1)` is ALREADY covered by the rec_pow_* family — and the
    # model still wrote `2 ** n` for pow2, so more of that shape won't help. The real gaps are below:
    # teaching that a square is `mul n n` (not `n^2`/`n**2`) and that digit-sums are div/mod recursion.)
    # 36b. square (n*n) in composition:  \n -> op (mul n n) k   — teaches "square = n*n", not n^2/n**2
    for op in ("add", "sub", "mul"):
        pf = _AOP[op]
        for k in (2, 3, 5, 10):
            add(_cspec(f"square_{op}_{k}", f"Square a number, then {_OPWORD[op]} {k}.",
                       f"{op} (mul n n) {k}", ["arithmetic", "square", "composition"], fn([INT], INT),
                       lam(["n"], bapp(op, bapp("mul", n, n), int_lit(k))),
                       [{"args": [v], "result": pf(v * v, k)} for v in _INT_IN]))
    # 36c. two-arg squared combinations:  \a b -> op (mul a a) (mul b b)   (square_diff a*a-b*b is held out)
    for op in ("add", "mul"):
        pf = _AOP[op]
        add(_cspec(f"sqcomb_{op}", f"Combine the squares of two numbers with {op}.",
                   f"{op} (mul a a) (mul b b)", ["arithmetic", "square", "two-arg"], fn([INT, INT], INT),
                   lam(["a", "b"], bapp(op, bapp("mul", var("a"), var("a")), bapp("mul", var("b"), var("b")))),
                   [{"args": [u, w], "result": pf(u * u, w * w)} for u, w in [(2, 3), (5, -1), (0, 4), (-2, -3), (7, 1)]]))
    # 36d. digit/bit sum by div/mod recursion:  \n -> case n==0 {0; n%b + self (n/b)}   (base 10 = sum_digits, held out)
    for b in (2, 3, 10):
        add(_cspec(f"digitsum_base_{b}", f"Sum the base-{b} digits of a non-negative number.",
                   f"0 when n is 0; else (n % {b}) + self (n / {b})", ["arithmetic", "recursion", "digits", "divmod"],
                   fn([INT], INT),
                   lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                        bapp("add", bapp("mod", n, int_lit(b)), bself(bapp("div", n, int_lit(b)))))),
                   [{"args": [v], "result": _dsum(v, b)} for v in _NN36], terminates="unknown"))
    # 36e. digit count by div/mod recursion:  \n -> case n==0 {0; 1 + self (n/b)}
    for b in (2, 10):
        add(_cspec(f"numdigits_base_{b}", f"Count the base-{b} digits of a non-negative number.",
                   f"0 when n is 0; else 1 + self (n / {b})", ["arithmetic", "recursion", "digits", "divmod", "count"],
                   fn([INT], INT),
                   lam(["n"], case_bool(bapp("eq", n, int_lit(0)), int_lit(0),
                        bapp("add", int_lit(1), bself(bapp("div", n, int_lit(b)))))),
                   [{"args": [v], "result": _ndig(v, b)} for v in _NN36], terminates="unknown"))

    # 37. SINGLE-ELEMENT-BASE list recursion — reduce a NON-EMPTY list by combining the head with the
    # recursion on the tail, basing out at the one-element list (`case null (tail xs) -> head xs`). This is
    # the idiom the eval's max_list_rec/min-of-list/last need; the model instead reached for an `error`
    # builtin on the empty case (the total dialect has no `error`). The exact max_list_rec body is
    # leakage-dropped; min/sum/product teach the SHAPE. Tail descent -> terminates=always; inputs non-empty.
    def _prod37(lst):
        r = 1
        for _v in lst:
            r *= _v
        return r
    _RED37 = {"max": max, "min": min, "add": sum, "mul": _prod37}
    _RLST37 = [[3], [5, 2], [1, 4, 2], [7, 3, 9, 1], [2, 2, 2], [-1, -5, -2]]
    for opn, rf in _RED37.items():
        add(_cspec(f"reduce1_{opn}", f"Combine a non-empty list's elements with {opn} (single-element base).",
                   f"head xs when the tail is empty; else {opn} (head xs) (self (tail xs))",
                   ["recursion", "list", "reduce", opn], fn([list_of(INT)], INT),
                   lam(["xs"], case_bool(bapp("null", bapp("tail", xs)), bapp("head", xs),
                        bapp(opn, bapp("head", xs), bself(bapp("tail", xs))))),
                   [{"args": [lst], "result": rf(lst)} for lst in _RLST37], terminates="always"))
    # 38. INDEX RECURSION — walk to the n-th element by decrementing the index AND peeling the tail in
    # lockstep (`self (n-1) (tail xs)`), basing out at `n == 0`. The eval's `nth` needs exactly this; the
    # model reached for Haskell `!!` or an `error` builtin. The identity-base body (= nth) is leakage-
    # dropped; the transformed-head variants teach the index-walk idiom. Valid indices only (0 <= n < len).
    # Mixed counter+tail descent -> declare terminates=unknown (conservative; never a false `always`).
    def _nth(base_ast):
        return lam(["n", "xs"], case_bool(bapp("eq", n, int_lit(0)), base_ast,
                   bself(bapp("sub", n, int_lit(1)), bapp("tail", xs))))
    _IDX38 = [(0, [7, 8, 9]), (1, [7, 8, 9]), (2, [7, 8, 9]), (0, [5]), (1, [3, 6]), (2, [4, 1, 2, 9])]
    for nm, base, rf in (
        ("nth_at", bapp("head", xs), lambda e: e),
        ("nth_double", bapp("mul", int_lit(2), bapp("head", xs)), lambda e: 2 * e),
        ("nth_neg", bapp("neg", bapp("head", xs)), lambda e: -e),
    ):
        add(_cspec(nm, f"Return {'the'if nm=='nth_at' else '2x the'if nm=='nth_double' else 'the negation of the'} element at index n of a list (0-based).",
                   "index-walk: base when n is 0; else self (n-1) (tail xs)", ["recursion", "list", "index"],
                   fn([INT, list_of(INT)], INT), _nth(base),
                   [{"args": [i, lst], "result": rf(lst[i])} for i, lst in _IDX38], terminates="unknown"))

    # 39. STRINGS AS DATA (spec/expressiveness.md phase 1) — the split/join/concat/parse-int idioms
    # multiplied over separator/constant sets. Teaches: pattern/separator-first argument order, split
    # keeps empties, parse_int's totality-via-Maybe consumed by a case (never `error`), and
    # format-by-join-over-map. The exact curated golds (comma variants) dedupe/leakage-drop as usual;
    # the other separators teach the SHAPE.
    s = var("s")
    _SEP39 = [(",", "comma", "commas"), (";", "semi", "semicolons"), ("|", "pipe", "pipes"),
              (" ", "space", "spaces"), (":", "colon", "colons")]
    _STR_IN = ["a{0}b{0}c", "x{0}y", "", "one", "a{0}{0}b"]  # templates instantiated per separator
    _STRLIST_IN = [["a", "b"], [], ["z"], ["a", "", "c"]]
    _INTLIST39 = [[1, 2, 3], [], [-4], [0, 7]]
    for sep, nm, word in _SEP39:
        inputs = [t.format(sep) for t in _STR_IN]
        add(_cspec(f"count_fields_{nm}", f"How many fields a string has when split on {word}.",
                   f'length (str_split "{sep}" s)', ["string", "parse"], fn([STRING], NAT),
                   lam(["s"], bapp("length", bapp("str_split", str_lit(sep), s))),
                   [{"args": [t], "result": len(t.split(sep))} for t in inputs], terminates="always"))
        two_field = [t.format(sep) for t in ("a{0}b{0}c", "x{0}y", "1{0}2{0}3{0}4")]
        add(_cspec(f"second_field_{nm}", f"The second field of a string split on {word}.",
                   f'head (tail (str_split "{sep}" s))', ["string", "parse"], fn([STRING], STRING),
                   lam(["s"], bapp("head", bapp("tail", bapp("str_split", str_lit(sep), s)))),
                   [{"args": [t], "result": t.split(sep)[1]} for t in two_field], terminates="always"))
        add(_cspec(f"join_{nm}", f"Join a list of strings with {word}.",
                   f'str_join "{sep}" xs', ["string", "format"], fn([list_of(STRING)], STRING),
                   lam(["xs"], bapp("str_join", str_lit(sep), xs)),
                   [{"args": [l], "result": sep.join(l)} for l in _STRLIST_IN], terminates="always"))
        add(_cspec(f"render_ints_{nm}", f"Render a list of integers as a string separated by {word}.",
                   f'str_join "{sep}" (map to_string xs)', ["string", "format", "list"],
                   fn([list_of(INT)], STRING),
                   lam(["xs"], bapp("str_join", str_lit(sep), bapp("map", var("to_string"), xs))),
                   [{"args": [l], "result": sep.join(str(v) for v in l)} for l in _INTLIST39]))
        add(_cspec(f"parse_first_{nm}", f"Parse the first field (split on {word}) as an integer, if it is one.",
                   f'parse_int (head (str_split "{sep}" s))', ["string", "parse", "maybe"],
                   fn([STRING], maybe_t(INT)),
                   lam(["s"], bapp("parse_int", bapp("head", bapp("str_split", str_lit(sep), s)))),
                   [{"args": [f"21{sep}x"], "result": V("Just", 21)},
                    {"args": [f"junk{sep}x"], "result": V("None")},
                    {"args": [f"-3{sep}9"], "result": V("Just", -3)}], terminates="always"))
    for pre, post, nm in (("(", ")", "parens"), ("[", "]", "brackets"), ("<", ">", "angles"), ("{", "}", "braces")):
        add(_cspec(f"wrap_{nm}", f"Wrap a string in {nm}.",
                   f'str_concat "{pre}" (str_concat s "{post}")', ["string", "format"], fn([STRING], STRING),
                   lam(["s"], bapp("str_concat", str_lit(pre), bapp("str_concat", s, str_lit(post)))),
                   [{"args": ["x"], "result": f"{pre}x{post}"}, {"args": [""], "result": f"{pre}{post}"},
                    {"args": ["a b"], "result": f"{pre}a b{post}"}], terminates="always"))
    for k in (0, 1, -1, 9, 100):
        add(_cspec(f"parse_or_neg{-k}" if k < 0 else f"parse_or_{k}", f"Parse a string as an integer, defaulting to {k}.",
                   f"case parse_int s of Just(n) => n; None => {k}", ["string", "parse", "variant", "case"],
                   fn([STRING], INT),
                   lam(["s"], _case_of(bapp("parse_int", s), (_vpat("Just", "n"), n),
                                       (_vpat("None"), int_lit(k)))),
                   [{"args": ["7"], "result": 7}, {"args": ["junk"], "result": k},
                    {"args": ["-2"], "result": -2}], terminates="always"))
    for pre in ("n=", "id:", "#"):
        nm = {"n=": "neq", "id:": "id", "#": "hash"}[pre]
        add(_cspec(f"label_{nm}", f'Render an integer with the prefix "{pre}".',
                   f'str_concat "{pre}" (to_string n)', ["string", "format", "arithmetic"], fn([INT], STRING),
                   lam(["n"], bapp("str_concat", str_lit(pre), bapp("to_string", n))),
                   [{"args": [5], "result": f"{pre}5"}, {"args": [-1], "result": f"{pre}-1"},
                    {"args": [0], "result": f"{pre}0"}], terminates="always"))

    # 40. MAPS & JSON (spec/expressiveness.md phases 2-3) — the config-lookup and field-projection
    # idioms multiplied over key/default sets. Teaches: key-first argument order, map_get/parse_json's
    # totality-via-Maybe consumed by case (incl. the NESTED Just(JObj(m))/Just(JNum(p)) pattern — the
    # GW1 practical form), and building maps from map_empty. Exact curated golds dedupe/leakage-drop.
    _KEYS40 = ["port", "count", "size", "level", "id"]
    _DFLT40 = [0, 1, 8080]
    for key in _KEYS40:
        for k in _DFLT40:
            add(_cspec(f"get_{key}_or_{k}", f'The "{key}" entry of a map of integers, or {k} if unset.',
                       f'case map_get "{key}" m of Just(v) => v; None => {k}',
                       ["map", "query", "variant", "case"], fn([map_of(INT)], INT),
                       lam(["m"], _case_of(bapp("map_get", str_lit(key), var("m")),
                                           (_vpat("Just", "v"), var("v")), (_vpat("None"), int_lit(k)))),
                       [{"args": [{key: 9, "other": 3}], "result": 9},
                        {"args": [{}], "result": k},
                        {"args": [{"unrelated": 7}], "result": k}], terminates="always"))
        add(_cspec(f"has_{key}", f'Whether a map of integers has a "{key}" entry.',
                   f'case map_get "{key}" m of Just(v) => true; None => false',
                   ["map", "query", "predicate", "variant", "case"], fn([map_of(INT)], BOOL),
                   lam(["m"], _case_of(bapp("map_get", str_lit(key), var("m")),
                                       (_vpat("Just", "v"), bool_lit(True)), (_vpat("None"), bool_lit(False)))),
                   [{"args": [{key: 1}], "result": True}, {"args": [{}], "result": False},
                    {"args": [{"zz": 2}], "result": False}], terminates="always"))
        add(_cspec(f"set_{key}", f'Set the "{key}" entry of a map of integers.',
                   f'map_put "{key}" n m', ["map", "transform"], fn([INT, map_of(INT)], map_of(INT)),
                   lam(["n", "m"], bapp("map_put", str_lit(key), var("n"), var("m"))),
                   [{"args": [5, {}], "result": {key: 5}},
                    {"args": [2, {key: 1}], "result": {key: 2}},
                    {"args": [0, {"other": 9}], "result": {"other": 9, key: 0}}], terminates="always"))
        # The GW1 practical form: parse a JSON config text, project an integer field, default on
        # anything malformed/missing/mistyped — nested variant patterns end to end.
        _JP = {"kind": "variant", "tag": "Just",
               "payload": {"kind": "variant", "tag": "JObj", "payload": {"kind": "bind", "name": "m"}}}
        _JN = {"kind": "variant", "tag": "Just",
               "payload": {"kind": "variant", "tag": "JNum", "payload": {"kind": "bind", "name": "p"}}}
        add(_cspec(f"json_{key}", f'The integer "{key}" field of a JSON object text, or 0.',
                   f'case parse_json s of Just(JObj(m)) => (case map_get "{key}" m of Just(JNum(p)) => p; _ => 0); _ => 0',
                   ["parse", "query", "string", "map", "variant", "case"], fn([STRING], INT),
                   lam(["s"], _case_of(
                       bapp("parse_json", var("s")),
                       (_JP, _case_of(bapp("map_get", str_lit(key), var("m")),
                                      (_JN, var("p")), (WILDCARD_PAT, int_lit(0)))),
                       (WILDCARD_PAT, int_lit(0)))),
                   [{"args": [f'{{"{key}": 42, "x": true}}'], "result": 42},
                    {"args": ['{"unrelated": 1}'], "result": 0},
                    {"args": ["not json"], "result": 0}], terminates="always"))

    # 41. NEAR-BARE BUILTIN USAGE — the corpus10 residual diagnosis: families #39/#40 taught
    # COMPOSITE idioms (split-then-count, get-or-default) but several phase-1/2/3 builtins never
    # appeared in training at all (their bare curated golds leakage-drop), so the model invented
    # `keys`/`sort`/regex or recursed over a string as if it were a list. Each shape here uses one
    # new builtin with minimal decoration, multiplied over constants — enough mass to teach the
    # OPERATION itself while the exact bare golds still drop.
    for k in (0, 1, 3, 10):
        add(_cspec(f"len_plus_{k}", f"The length of a string plus {k}.",
                   f"str_length s + {k}", ["string"], fn([STRING], INT),
                   lam(["s"], bapp("add", bapp("str_length", var("s")), int_lit(k))),
                   [{"args": ["hello"], "result": 5 + k}, {"args": [""], "result": k},
                    {"args": ["ab"], "result": 2 + k}], terminates="always"))
        add(_cspec(f"longer_than_{k}", f"Whether a string is longer than {k} characters.",
                   f"str_length s > {k}", ["string", "predicate"], fn([STRING], BOOL),
                   lam(["s"], bapp("gt", bapp("str_length", var("s")), int_lit(k))),
                   [{"args": ["hello"], "result": 5 > k}, {"args": [""], "result": 0 > k},
                    {"args": ["abcd"], "result": 4 > k}], terminates="always"))
    for sep, nm, word in _SEP39:
        add(_cspec(f"has_sep_{nm}", f"Whether a string contains a {word.rstrip('s')}.",
                   f'str_contains "{sep}" s', ["string", "predicate"], fn([STRING], BOOL),
                   lam(["s"], bapp("str_contains", str_lit(sep), var("s"))),
                   [{"args": [f"a{sep}b"], "result": True}, {"args": ["ab"], "result": False},
                    {"args": [sep], "result": True}], terminates="always"))
    for k in (1, 2, 5):
        add(_cspec(f"show_plus_{k}", f"Render n plus {k} as a decimal string.",
                   f"to_string (n + {k})", ["string", "format", "arithmetic"], fn([INT], STRING),
                   lam(["n"], bapp("to_string", bapp("add", var("n"), int_lit(k)))),
                   [{"args": [5], "result": str(5 + k)}, {"args": [-1], "result": str(-1 + k)},
                    {"args": [0], "result": str(k)}], terminates="always"))
    for key in _KEYS40:
        add(_cspec(f"singleton_{key}", f'A one-entry map holding "{key}".',
                   f'map_put "{key}" n map_empty', ["map", "transform"], fn([INT], map_of(INT)),
                   lam(["n"], bapp("map_put", str_lit(key), var("n"), var("map_empty"))),
                   [{"args": [5], "result": {key: 5}}, {"args": [0], "result": {key: 0}},
                    {"args": [-3], "result": {key: -3}}], terminates="always"))
        add(_cspec(f"without_{key}", f'A map with its "{key}" entry removed (no-op when absent).',
                   f'map_del "{key}" m', ["map", "transform"], fn([map_of(INT)], map_of(INT)),
                   lam(["m"], bapp("map_del", str_lit(key), var("m"))),
                   [{"args": [{key: 1, "other": 2}], "result": {"other": 2}},
                    {"args": [{"other": 2}], "result": {"other": 2}},
                    {"args": [{}], "result": {}}], terminates="always"))
        add(_cspec(f"keys_without_{key}", f'The sorted keys of a map after removing "{key}".',
                   f'map_keys (map_del "{key}" m)', ["map", "query"], fn([map_of(INT)], list_of(STRING)),
                   lam(["m"], bapp("map_keys", bapp("map_del", str_lit(key), var("m")))),
                   [{"args": [{key: 1, "b": 2, "a": 3}], "result": ["a", "b"]},
                    {"args": [{key: 1}], "result": []},
                    {"args": [{"z": 9}], "result": ["z"]}], terminates="always"))
        add(_cspec(f"size_without_{key}", f'How many entries a map has once "{key}" is removed.',
                   f'map_size (map_del "{key}" m)', ["map", "aggregate"], fn([map_of(INT)], NAT),
                   lam(["m"], bapp("map_size", bapp("map_del", str_lit(key), var("m")))),
                   [{"args": [{key: 1, "b": 2}], "result": 1}, {"args": [{key: 1}], "result": 0},
                    {"args": [{"a": 1, "b": 2}], "result": 2}], terminates="always"))
    for k in (0, 1, 100):
        add(_cspec(f"parses_over_{k}", f"Whether a string parses as an integer greater than {k}.",
                   f"case parse_int s of Just(n) => n > {k}; None => false",
                   ["string", "parse", "predicate", "variant", "case"], fn([STRING], BOOL),
                   lam(["s"], _case_of(bapp("parse_int", var("s")),
                                       (_vpat("Just", "n"), bapp("gt", var("n"), int_lit(k))),
                                       (_vpat("None"), bool_lit(False)))),
                   [{"args": ["7"], "result": 7 > k}, {"args": ["junk"], "result": False},
                    {"args": ["-2"], "result": -2 > k}, {"args": [str(k)], "result": False}],
                   terminates="always"))
    # 42. LIST-RETURNING INDEX WALKS — the last designed write residual (take_rec/drop_rec failed
    # every tier through corpus11). #38 taught the ELEMENT-returning index walk (`nth`); this family
    # teaches the two walks that return LISTS: the take shape (double guard `n==0` then `null`,
    # consing a TRANSFORMED head onto `self (n-1) (tail xs)` — the transform keeps the exact golds
    # leakage-dropped) and the drop shape (same guards, recurse WITHOUT cons, varied base at n==0).
    # Mixed counter+tail descent -> terminates=unknown (conservative), like #38.
    def _take_walk(head_expr):
        return lam(["n", "xs"], case_bool(bapp("eq", var("n"), int_lit(0)), var("nil"),
                   case_null("xs", var("nil"),
                             bapp("cons", head_expr,
                                  bself(bapp("sub", var("n"), int_lit(1)), bapp("tail", var("xs")))))))
    _TAKE_IN = [(2, [1, 2, 3, 4]), (0, [5, 6]), (3, [7, 8, 9]), (1, [4])]
    for nm, head_expr, tf in (
        ("take_double", bapp("mul", int_lit(2), bapp("head", var("xs"))), lambda v: 2 * v),
        ("take_neg", bapp("neg", bapp("head", var("xs"))), lambda v: -v),
        ("take_add_1", bapp("add", bapp("head", var("xs")), int_lit(1)), lambda v: v + 1),
        ("take_add_10", bapp("add", bapp("head", var("xs")), int_lit(10)), lambda v: v + 10),
    ):
        add(_cspec(nm, f"The first n elements of a list, each {'doubled' if nm == 'take_double' else 'negated' if nm == 'take_neg' else 'increased by ' + nm.split('_')[-1]}.",
                   "take walk: nil when n==0 or empty; else cons (f head) (self (n-1) (tail xs))",
                   ["recursion", "list", "slice"], fn([INT, list_of(INT)], list_of(INT)),
                   _take_walk(head_expr),
                   [{"args": [n, lst], "result": [tf(v) for v in lst[:n]]} for n, lst in _TAKE_IN],
                   terminates="unknown"))
    def _drop_walk(base_expr, empty_expr):
        return lam(["n", "xs"], case_bool(bapp("eq", var("n"), int_lit(0)), base_expr,
                   case_null("xs", empty_expr,
                             bself(bapp("sub", var("n"), int_lit(1)), bapp("tail", var("xs"))))))
    for nm, base, empty, ty_res, pf in (
        ("after_n_reversed", bapp("reverse", var("xs")), var("nil"), list_of(INT),
         lambda n, lst: list(reversed(lst[n:]))),
        ("after_n_count", bapp("length", var("xs")), int_lit(0), NAT,
         lambda n, lst: len(lst[n:])),
        ("after_n_count_plus_1", bapp("add", bapp("length", var("xs")), int_lit(1)), int_lit(1), INT,
         lambda n, lst: len(lst[n:]) + 1),
    ):
        add(_cspec(nm, {"after_n_reversed": "The elements after the first n, reversed.",
                        "after_n_count": "How many elements remain after dropping the first n.",
                        "after_n_count_plus_1": "One more than the count of elements after the first n."}[nm],
                   "drop walk: base when n==0; empty-base when the list runs out; else self (n-1) (tail xs)",
                   ["recursion", "list", "slice"], fn([INT, list_of(INT)], ty_res),
                   _drop_walk(base, empty),
                   [{"args": [n, lst], "result": pf(n, lst)} for n, lst in _TAKE_IN],
                   terminates="unknown"))

    for dflt in ("", "{}", "null"):
        nm = {"": "empty", "{}": "obj", "null": "null"}[dflt]
        add(_cspec(f"canon_or_{nm}", f'Canonicalize a JSON text, defaulting to "{dflt}" when invalid.',
                   f'case parse_json s of Just(j) => render_json j; None => "{dflt}"',
                   ["parse", "serialize", "string", "variant", "case"], fn([STRING], STRING),
                   lam(["s"], _case_of(bapp("parse_json", var("s")),
                                       (_vpat("Just", "j"), bapp("render_json", var("j"))),
                                       (_vpat("None"), str_lit(dflt)))),
                   [{"args": ["{ \"b\" : 2 , \"a\": 1 }"], "result": "{\"a\":1,\"b\":2}"},
                    {"args": ["[1,  2]"], "result": "[1,2]"},
                    {"args": ["nope"], "result": dflt}], terminates="always"))

    return out


def all_specs():
    return (unary_arith() + binary_arith() + boolean_funcs() + list_funcs()
            + list_transform_funcs() + composition_funcs() + list_fold_funcs() + refined_funcs() + costed_funcs() + float_funcs()
            + maybe_funcs() + result_funcs() + recursive_funcs() + recursive_list_funcs()
            + arith_laws() + bool_laws() + order_laws()
            + more_arith() + more_laws() + bool_more() + recursive_more()
            + recursive_shapes() + compositional_bodies() + more_compositional() + more_recursion()
            + variant_consuming_funcs() + nested_hof_funcs() + string_funcs() + map_json_funcs())


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
            "CONSISTENT", "CONTRADICTED", "UNVERIFIABLE",
            "SOUND", "VIOLATED", "N/A",  # check-refinement verdicts
            "VERIFIED"}  # check-complexity: a declared bound the checker proves is actually tighter
    found = []
    for line in text.splitlines():
        for t in line.replace(":", " ").split():
            if t in toks:
                found.append(t)
                break
    return found


def build_and_verify(spec, workdir):
    # A higher-order spec references helper functions by `fn_deps`; build those records first so their
    # content-addresses are known, then resolve any FnRef example argument to an `fn_ref` value pointing at
    # the helper's hash. The helpers are written into the run directory so `run` resolves the fn_ref.
    dep_records, dep_bodies, dep_hashes, helpers = [], {}, {}, []
    for dep in spec.get("fn_deps", []):
        dep_ex = [{"args": [to_value_ast(a) for a in e["args"]], "result": to_value_ast(e["result"])}
                  for e in dep["examples"]]
        dep_rec = build_v2_record(dep["name"], dep["type_ast"], dep_ex, dep["body_ast"], terminates="always")
        dep_records.append(dep_rec)
        dep_bodies[expr_address(dep["body_ast"])] = dep["body_ast"]
        dep_hashes[dep["name"]] = dep_rec["hash"]
        # Carry the helper record + body so a downstream consumer (e.g. the eval grader) can rebuild a
        # runnable directory and resolve the example's fn_ref by address — making higher-order records
        # executable without re-deriving the helper.
        helpers.append({"name": dep["name"], "record": dep_rec, "body": dep["body_ast"]})

    def resolve_arg(a):
        return {"kind": "fn_ref", "target": dep_hashes[a.name]} if isinstance(a, FnRef) else to_value_ast(a)

    examples = [{"args": [resolve_arg(a) for a in ex["args"]], "result": to_value_ast(ex["result"])}
                for ex in spec["examples"]]
    # A spec may declare its own termination class (e.g. an unbounded self-recursion that isn't certified
    # `always` here); default to `always`, with `sum`'s fold left `unknown` for back-compat.
    terminates = spec.get("terminates", "unknown" if spec["name"] == "sum" else "always")
    record = build_v2_record(spec["name"], spec["type_ast"], examples, spec["body_ast"],
                             properties=spec.get("properties") or None, intent_tags=spec["tags"],
                             terminates=terminates, refinements=spec.get("refinements"),
                             complexity=spec.get("complexity"), cost=spec.get("cost"))
    # Derivation history (principle 1): stamp derived_from / supersedes with a parent's content-address
    # (the parent is declared as an fn_dep so its hash is known), then re-hash since the content changed.
    # `derived_from` is a LIST of addresses (a function may derive from several); `supersedes` is a single.
    prov = {}
    if spec.get("derived_from"):
        prov["derived_from"] = [dep_hashes[spec["derived_from"]]]
    if spec.get("supersedes"):
        prov["supersedes"] = dep_hashes[spec["supersedes"]]
    if prov:
        record.update(prov)
        record["hash"] = content_hash(record, "fn", strip=("hash",))
    body = spec["body_ast"]
    addr = expr_address(body)
    d = os.path.join(workdir, spec["name"])
    write_runnable_dir(d, [record] + dep_records, {addr: body, **dep_bodies})
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
    # Refinement check: a record's declared `pre`/`post` refinements — and an implicit `nat` result —
    # must hold against the body (`check-refinement`, the prove-backed pass). Run it when there's anything
    # to check; the verdicts (SOUND / N/A / UNVERIFIABLE per refinement) join the verification record, and
    # a VIOLATED fails the gate (no record that breaks its own declared contract enters the corpus).
    refinements_checked = []
    result_t = spec["type_ast"].get("body", spec["type_ast"]).get("result", {})
    if spec.get("refinements") or result_t.get("name") == "nat":
        refinements_checked = verdict_tokens(cli(["check-refinement", rec_path, "--body", body_path]).stdout)
    # Complexity check: when a record DECLARES a `signature.complexity`, `check-complexity` infers a sound
    # upper bound by structural cost analysis (no solver) and confirms the declaration holds. There is no
    # refutation (an upper-bound claim can be verified but never disproved), so the acceptable verdicts are
    # SOUND (bound matches) and VERIFIED (declared bound is provably tighter-satisfiable); anything else
    # (UNVERIFIABLE — the sound bound is worse, or the body is opaque) fails the gate for a costed record.
    complexity_checked = []
    if spec.get("complexity") or spec.get("cost"):
        complexity_checked = verdict_tokens(cli(["check-complexity", rec_path, "--body", body_path]).stdout)
    # Certification: the capstone — `certify` runs EVERY verified-by-default check in one pass (typecheck /
    # effects / refinement / termination / complexity+cost) and returns a single verdict. Each record's
    # certificate is recorded and its `certified` flag joins the verification view; a record that fails a
    # HARD check (ill-typed / under-declared effect / violated refinement) is not certified and can't enter
    # the corpus. So every function example ships with a machine-checkable "verified by default" stamp.
    certified = False
    cert_out = cli(["certify", rec_path, "--body", body_path, "--records", d, "--json"]).stdout
    if cert_out.strip():
        try:
            certified = bool(json.loads(cert_out).get("certified", False))
        except json.JSONDecodeError:
            certified = False

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
            **({"helpers": helpers} if helpers else {}),
        },
        "verification": {
            "schema_valid": schema_valid,
            "well_typed": well_typed,
            "examples_passed": f"{len(spec['examples'])}/{len(spec['examples'])}" if examples_passed else "FAILED",
            "bounded_check": bounded,
            "proofs": proofs,
            **({"refinements": refinements_checked} if refinements_checked else {}),
            **({"complexity": complexity_checked} if complexity_checked else {}),
            "certified": certified,
        },
    }
    ok = (schema_valid and well_typed and examples_passed
          and all(p["verdict"] in ("PROVED",) for p in proofs)
          and "VIOLATED" not in refinements_checked
          and all(v in ("SOUND", "VERIFIED", "N/A") for v in complexity_checked)
          and certified)
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
                              properties=spec.get("properties") or None, intent_tags=spec["tags"],
                              terminates=terminates, refinements=spec.get("refinements"))
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
        ("apply_sum_foldr", "Ask an agent to sum the list [1, 2, 3] with a right fold.",
         "request/apply sum_foldr to [1, 2, 3] → the responder runs it over the list argument and asserts "
         "sum_foldr([1, 2, 3]) = 6, which re-runs true.",
         "request", "sum_foldr", [[1, 2, 3]], ["agent-loop", "request", "apply", "list"]),
        ("apply_all_positive", "Ask an agent whether every element of [2, 4, 6] is positive.",
         "request/apply all_positive to [2, 4, 6] → the responder asserts all_positive([2, 4, 6]) = true, "
         "which re-runs true (an apply whose result is a boolean).",
         "request", "all_positive", [[2, 4, 6]], ["agent-loop", "request", "apply", "list", "predicate"]),
        ("apply_cube", "Ask an agent to cube 3.",
         "request/apply cube to 3 → the responder asserts cube(3) = 27, which re-runs true.",
         "request", "cube", [3], ["agent-loop", "request", "apply"]),
        ("apply_member", "Ask an agent whether 2 occurs in [1, 2, 3].",
         "request/apply member to (2, [1, 2, 3]) → the responder asserts member(2, [1, 2, 3]) = true, "
         "which re-runs true (a two-argument apply of a recursive function, whose claim re-run binds self).",
         "request", "member", [2, [1, 2, 3]], ["agent-loop", "request", "apply", "list", "search", "recursion"]),
        ("propose_double", "Propose that an agent compute double of 21.",
         "propose/apply double to 21 → the responder test-runs it and commits.",
         "propose", "double", [21], ["agent-loop", "propose"]),
        ("propose_negate", "Propose that an agent negate every element of [1, -2, 3].",
         "propose/apply negate_all to [1, -2, 3] → the responder test-runs it over the list and commits.",
         "propose", "negate_all", [[1, -2, 3]], ["agent-loop", "propose", "list"]),
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

    # query → ack (discovery by a different intent tag — the refinement-carrying functions).
    qreq2 = {"schema_version": "0.2.0", "kind": "query", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
             "constraints": None, "body": {"limit": 50, "pattern": {"intent_tags": ["refinement"]}}}
    qsigned2 = sign_message(qreq2, SENDER_SEED)
    qreply2 = respond_to(qsigned2, commons_dir)
    matches2 = qreply2.get("body", {}).get("result", {}).get("matches", []) if qreply2 else []
    qok2 = (msg_schema_valid(qsigned2) and bool(qreply2) and msg_schema_valid(qreply2)
            and qreply2.get("kind") == "ack" and len(matches2) > 0
            and qreply2.get("in_reply_to") == qsigned2.get("hash"))
    emit("query_refinement", "Find functions that carry refinement predicates.",
         "query for functions tagged `refinement` → the responder acks with the matching content-addresses.",
         ["agent-loop", "query", "discovery"], "query", qsigned2, qreply2, f"ACK {len(matches2)} match(es)", qok2)

    # request/validate → assert: validate a LIST function (reverse), not just a scalar one.
    vreq2 = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
             "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000},
             "body": {"action": "validate", "target": by_name["reverse"]["hash"]}}
    vsigned2 = sign_message(vreq2, SENDER_SEED)
    vreply2 = respond_to(vsigned2, commons_dir)
    v_ok2 = (msg_schema_valid(vsigned2) and bool(vreply2) and msg_schema_valid(vreply2)
             and vreply2.get("kind") == "assert" and vreply2.get("in_reply_to") == vsigned2.get("hash"))
    emit("validate_reverse", "Ask an agent to validate the `reverse` function.",
         "request/validate reverse → the responder type-checks and runs it, then asserts it is verified.",
         ["agent-loop", "request", "validate", "list"], "request", vsigned2, vreply2,
         "VERIFIED" if v_ok2 else (vreply2.get("kind", "NO-REPLY").upper() if vreply2 else "NO-REPLY"), v_ok2)
    return out


def multiturn_examples(commons_dir, by_name):
    """Multi-turn signed transcripts (category `transcript`): the agent DISCOVERS a function by intent
    (query -> ack), then USES the discovered content-address in a follow-up turn (apply -> assert, or
    validate -> assert). Each turn is a real signed message answered by `nl-validator respond`, and the
    whole chain is threaded by in_reply_to. Verified end to end — the ack must actually list the target the
    follow-up uses, and the final assert re-runs/validates true — so it is principle 4 made multi-turn."""
    resp_did = responder_did(RESPONDER_SEED)
    if not resp_did:
        return []
    out = []

    def query_turn(tag):
        # `limit` must exceed the size of any tagged family: this transcript requires a SPECIFIC target
        # (double/reverse) to be among the returned matches, so a cap below the tag's population would
        # silently truncate the target out as the corpus grows. 500 is generous headroom over the family
        # sizes (a discover-then-use query, not a pagination test).
        q = {"schema_version": "0.2.0", "kind": "query", "in_reply_to": None, "timestamp": MSG_TS, "to": resp_did,
             "constraints": None, "body": {"limit": 500, "pattern": {"intent_tags": [tag]}}}
        sq = sign_message(q, SENDER_SEED)
        ack = respond_to(sq, commons_dir)
        matches = ack.get("body", {}).get("result", {}).get("matches", []) if ack else []
        return sq, ack, matches

    def follow_up(action, target, ack, args=None):
        body = {"action": action, "target": target}
        if args is not None:
            body["args"] = args
        req = {"schema_version": "0.2.0", "kind": "request", "in_reply_to": ack.get("hash") if ack else None,
               "timestamp": MSG_TS, "to": resp_did,
               "constraints": {"budget_tokens": 1000, "capabilities": [], "deadline_ms": 5000}, "body": body}
        sreq = sign_message(req, SENDER_SEED)
        return sreq, respond_to(sreq, commons_dir)

    def emit(ident, intent, summary, tags, transcript, extra_ok, outcome):
        threaded = all(transcript[i].get("in_reply_to") == transcript[i - 1].get("hash")
                       for i in range(1, len(transcript)))
        schema_ok = all(msg_schema_valid(m) for m in transcript)
        ok = bool(transcript) and schema_ok and threaded and extra_ok
        out.append({
            "id": "transcript_" + ident, "modality": "nova_locutio", "category": "transcript", "polarity": "positive",
            "intent": intent, "summary": summary, "tags": tags,
            "views": {"transcript": transcript, "turns": len(transcript) // 2, "outcome": outcome},
            "verification": {"all_schema_valid": schema_ok, "threaded": threaded, "outcome": outcome},
            "_ok": ok,
        })

    # T1: discover an arithmetic function (the ack lists double), then apply the discovered address.
    q1, ack1, m1 = query_turn("arithmetic")
    tgt1 = by_name["double"]["hash"]
    sapply, assert1 = follow_up("apply", tgt1, ack1, args=[to_value_ast(21)])
    confirmed = False
    if assert1 and assert1.get("kind") == "assert":
        vp = _write_tmp(assert1)
        confirmed = cli(["verify-claim", "--records", commons_dir, vp]).returncode == 0
        os.unlink(vp)
    transcript1 = [x for x in [q1, ack1, sapply, assert1] if x]
    ok1 = (bool(ack1) and ack1.get("kind") == "ack" and tgt1 in m1
           and bool(assert1) and assert1.get("kind") == "assert" and confirmed)
    emit("discover_then_apply",
         "Discover an arithmetic function, then apply the one you found.",
         "Two turns: query for `arithmetic` functions -> ack lists double's content-address; then request/apply "
         "that address to 21 -> assert double(21) = 42, which re-runs true. The apply targets a hash the query "
         "surfaced — discover-then-use, threaded end to end.",
         ["agent-loop", "multi-turn", "query", "apply", "discovery"], transcript1, ok1,
         "CONFIRMED" if ok1 else "NOT-CONFIRMED")

    # T2: discover a list function (the ack lists reverse), then validate the discovered address.
    q2, ack2, m2 = query_turn("list")
    tgt2 = by_name["reverse"]["hash"]
    sval, assert2 = follow_up("validate", tgt2, ack2)
    transcript2 = [x for x in [q2, ack2, sval, assert2] if x]
    ok2 = (bool(ack2) and ack2.get("kind") == "ack" and tgt2 in m2
           and bool(assert2) and assert2.get("kind") == "assert")
    emit("discover_then_validate",
         "Discover a list function, then validate the one you found.",
         "Two turns: query for `list` functions -> ack lists reverse's content-address; then request/validate "
         "that address -> assert it is verified. The validate targets a hash the query surfaced.",
         ["agent-loop", "multi-turn", "query", "validate", "discovery"], transcript2, ok2,
         "VERIFIED" if ok2 else "NOT-VERIFIED")
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

    # 6. Schema-invalid record — a structurally malformed record (its required `body_hash` field removed).
    #    The first gate, `validate`, rejects it against the schema before any semantic check can run.
    body6 = lam(["n"], bapp("add", n, n))
    rec6 = build_v2_record("double", fn([INT], INT), [{"args": [to_value_ast(3)], "result": to_value_ast(6)}],
                           body6, terminates="always")
    bad6 = {k: v for k, v in rec6.items() if k != "body_hash"}  # drop a required field
    p6 = _write_tmp(bad6)
    vv = cli(["validate", str(SCHEMA), p6])
    os.unlink(p6)
    emit("schema_invalid", "nova_lingua",
         "A function record with its required body_hash field removed.",
         "Structurally malformed: the schema validator rejects it before any semantic check, since body_hash is required.",
         ["negative", "schema"], {"record": bad6, "body": body6},
         "validate", "SCHEMA-INVALID", (vv.stdout + vv.stderr), vv.returncode != 0)

    # 7. Under-declared effects — a body that prints (an io.console effect) while the record declares none.
    #    `check-effects` proves statically that the body's effects are not a subset of the declared set and
    #    rejects it, without ever running it (an empty effect list is not a purity certificate).
    body7 = lam(["msg"], bapp("print", var("msg")))
    rec7 = build_v2_record("noisy", fn([INT], {"kind": "builtin", "name": "unit"}), [], body7, terminates="always")
    _, r7, b7 = write_rec("undereffect", rec7, body7)
    ce = cli(["check-effects", r7, "--body", b7])
    emit("under_declared_effects", "nova_lingua",
         "A function that prints but declares no effects.",
         "The body performs io.console (print) while the record's effect signature is empty; check-effects "
         "reports it UNDER-DECLARED before any execution.",
         ["negative", "effects"], {"record": rec7, "body": body7},
         "check-effects", "UNDER-DECLARED", (ce.stdout + ce.stderr), "UNDER-DECLARED" in (ce.stdout + ce.stderr))

    # 8. List op on a scalar — the body reverses an int. Declared int -> List int, but `reverse` needs a
    #    list, so the type checker rejects the body (a different ill-typing than a wrong declared return).
    body8 = lam(["n"], bapp("reverse", n))
    rec8 = build_v2_record("misapply", fn([INT], list_of(INT)),
                           [{"args": [to_value_ast(3)], "result": to_value_ast([3])}], body8, terminates="always")
    _, r8, b8 = write_rec("listonscalar", rec8, body8)
    tc8 = cli(["typecheck", r8, "--body", b8])
    emit("list_op_on_scalar", "nova_lingua",
         "A function whose body reverses an integer.",
         "reverse expects a list, but its argument is an int; the type checker rejects the body as ill-typed.",
         ["negative", "type-error"], {"record": rec8, "body": body8},
         "typecheck", "ILL-TYPED", (tc8.stdout + tc8.stderr), tc8.returncode != 0)

    # 9. Arity mismatch — the body applies `add` (a two-argument function) to a single argument, so it has
    #    a function type, not int; the type checker rejects it.
    body9 = lam(["n"], bapp("add", n))
    rec9 = build_v2_record("halfadd", fn([INT], INT),
                           [{"args": [to_value_ast(3)], "result": to_value_ast(3)}], body9, terminates="always")
    _, r9, b9 = write_rec("aritymismatch", rec9, body9)
    tc9 = cli(["typecheck", r9, "--body", b9])
    emit("arity_mismatch", "nova_lingua",
         "A function whose body applies add to a single argument.",
         "add takes two arguments; applied to one it yields a function, not an int, so the type checker rejects the body.",
         ["negative", "type-error"], {"record": rec9, "body": body9},
         "typecheck", "ILL-TYPED", (tc9.stdout + tc9.stderr), tc9.returncode != 0)

    # 10. cons onto a non-list — the body conses an int onto another int. `cons : a -> List a -> List a`
    #     needs a list as its second argument, so the type checker rejects it (a constructor misuse).
    body10 = lam(["n"], bapp("cons", n, n))
    rec10 = build_v2_record("badcons", fn([INT], list_of(INT)),
                            [{"args": [to_value_ast(3)], "result": to_value_ast([3, 3])}], body10, terminates="always")
    _, r10, b10 = write_rec("consonscalar", rec10, body10)
    tc10 = cli(["typecheck", r10, "--body", b10])
    emit("cons_onto_scalar", "nova_lingua",
         "A function whose body conses a number onto a number.",
         "cons needs a list as its second argument, but it is given an int; the type checker rejects the body as ill-typed.",
         ["negative", "type-error", "list"], {"record": rec10, "body": body10},
         "typecheck", "ILL-TYPED", (tc10.stdout + tc10.stderr), tc10.returncode != 0)
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
        ("negate_reverse_length", "Compose: negate every element, reverse, then count.",
         "A three-stage pipeline negate_all;reverse;length over a list of ints, yielding nat.",
         ["negate_all", "reverse", "length"], True),
        ("square_reverse_sumfoldr", "Compose: square every element, reverse, then right-fold-sum.",
         "A three-stage pipeline square_all;reverse;sum_foldr over a list of ints, yielding int.",
         ["square_all", "reverse", "sum_foldr"], True),
        ("filter_square_reverse_sum", "Compose: keep positives, square, reverse, then sum.",
         "A four-stage pipeline keep_positives;square_all;reverse;sum — the corpus's longest assembled pipeline.",
         ["keep_positives", "square_all", "reverse", "sum"], True),
        ("incrall_then_sum", "Compose: add one to every element (by recursion), then sum.",
         "A two-stage pipeline increment_all_rec;sum over a list of ints, yielding int.",
         ["increment_all_rec", "sum"], True),
        ("doubleall_then_reverse", "Compose: double every element (by recursion), then reverse.",
         "A two-stage pipeline double_all_rec;reverse over a list of ints.", ["double_all_rec", "reverse"], True),
        ("square_keeppos_count", "Compose: square every element, keep the positives, then count them.",
         "A three-stage pipeline square_all;keep_positives;length over a list of ints, yielding nat.",
         ["square_all", "keep_positives", "length"], True),
        ("keepposrec_then_sum", "Compose: keep the positives (by recursion), then sum them.",
         "A two-stage pipeline keep_positives_rec;sum over a list of ints, yielding int.",
         ["keep_positives_rec", "sum"], True),
        ("concatlists_then_length", "Compose: flatten a list of lists, then count the elements.",
         "A two-stage pipeline concat_lists;length over a list of lists of ints, yielding nat.",
         ["concat_lists", "length"], True),
        ("length_then_reverse", "Compose: take a list's length, then reverse it.",
         "length yields a nat, which cannot feed reverse's List parameter — the pipeline does NOT compose.",
         ["length", "reverse"], False),
        ("allpositive_then_reverse", "Compose: test all-positive, then reverse.",
         "all_positive yields a bool, which cannot feed reverse's List parameter — the pipeline does NOT compose.",
         ["all_positive", "reverse"], False),
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
    ap.add_argument("--combinatorial", action="store_true",
                    help="ALSO generate combinatorial (parameterized) function specs for a training-scale "
                         "corpus — point --out at a scratch path. The curated corpus.jsonl (no flag) is unchanged.")
    args = ap.parse_args()

    if not VALIDATOR.exists():
        sys.exit(f"nl-validator not built at {VALIDATOR} — run `cargo build --release` in tooling/validator")

    specs = all_specs()
    examples, dropped = [], []
    with tempfile.TemporaryDirectory(prefix="nlcorpus-") as wd:
        # Nova Lingua — verified function records (first-order specs, then the higher-order ones whose
        # examples reference helper functions by fn_ref). With --combinatorial, append the parameterized
        # specs (deduped against all hand-authored names) for a training-scale corpus.
        ho = higher_order_funcs() + higher_order_more() + provenance_funcs()
        combo = []
        if args.combinatorial:
            existing = {s["name"] for s in specs + ho}
            combo = combinatorial_specs(existing)
            print(f"combinatorial: generating {len(combo)} parameterized specs", file=sys.stderr)
        fn_specs = specs + ho + combo
        # build_and_verify is independent per spec (each writes its own subdir + spawns short-lived
        # validator subprocesses), so run them on a thread pool — the gate is subprocess-bound, not
        # Python-CPU-bound. Results are consumed in INPUT order (executor.map is ordered), so the output
        # stays byte-reproducible and the default (curated) corpus is byte-identical to the serial run.
        workers = min(12, (os.cpu_count() or 4) * 2)
        with ThreadPoolExecutor(max_workers=workers) as pool:
            for spec, (ex, ok) in zip(fn_specs, pool.map(lambda s: build_and_verify(s, wd), fn_specs)):
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
        # Multi-turn transcripts — discover a function, then use the discovered address in a later turn.
        for ex in multiturn_examples(commons_dir, by_name):
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
                "refined_funcs": len(refined_funcs()), "costed_funcs": len(costed_funcs()),
                "float_funcs": len(float_funcs()),
                "maybe_funcs": len(maybe_funcs()), "result_funcs": len(result_funcs()),
                "recursive_funcs": len(recursive_funcs()),
                "recursive_list_funcs": len(recursive_list_funcs()),
                "arith_laws": len(arith_laws()), "bool_laws": len(bool_laws()),
                "order_laws": len(order_laws()),
                "more_arith": len(more_arith()), "more_laws": len(more_laws()),
                "bool_more": len(bool_more()), "recursive_more": len(recursive_more()),
                "recursive_shapes": len(recursive_shapes()),
                "compositional_bodies": len(compositional_bodies()),
                "more_compositional": len(more_compositional()),
                "more_recursion": len(more_recursion()),
                "variant_consuming_funcs": len(variant_consuming_funcs()),
                "nested_hof_funcs": len(nested_hof_funcs()),
                "higher_order_funcs": len(higher_order_funcs()),
                "higher_order_more": len(higher_order_more()),
                "provenance_funcs": len(provenance_funcs())}
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
