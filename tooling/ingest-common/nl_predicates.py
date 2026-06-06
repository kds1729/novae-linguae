"""Nova Lingua predicate-expression AST builder (spec/predicate-expression.schema.json).

Used for `signature.refinements[].expr` and `properties[].expr` in v0.2 function records. Five node
kinds — var, lit, app, forall, exists — where comparisons, boolean connectives, and arithmetic are all
`app` with a closed-vocabulary `op` (eq/neq/lt/le/gt/ge, and/or/not/implies/iff, add/sub/mul/div/mod/neg,
length/head/tail/..., id/compose).

This module's neutral constructors are language-agnostic; `predicate_from_py` is the Python front-end
that maps a Python boolean/comparison/arithmetic expression (e.g. an `assert` condition used as a
precondition) to a predicate AST. Anything outside the supported forms raises PredicateError so the
caller skips it — nothing is fabricated. Binary ops are emitted as arity-2 `app`s (the verifier checks
op arity), and chained/variadic forms (`0 < x < 10`, `a and b and c`) are expanded into nested binaries.
"""

import ast
import re

_VAR = re.compile(r"^[a-z_][a-zA-Z0-9_']*$")
_CMP = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq", ast.NotEq: "neq"}
_BIN = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div", ast.Mod: "mod"}


class PredicateError(Exception):
    pass


def p_var(name):
    if not _VAR.match(name):
        raise PredicateError(f"{name!r} is not a valid predicate variable name")
    return {"kind": "var", "name": name}


def p_lit(value):
    return {"kind": "lit", "value": value}


def p_app(op, args):
    return {"kind": "app", "op": op, "args": args}


def p_forall(vars_, body):
    return {"kind": "forall", "vars": sorted(set(vars_)), "body": body}


def _fold(op, terms):
    """Right-fold >=1 terms into nested binary `app(op, [a, b])` (so op arity stays 2)."""
    acc = terms[-1]
    for t in reversed(terms[:-1]):
        acc = p_app(op, [t, acc])
    return acc


def predicate_from_py(node):
    """Map a Python expression AST node to a Nova Lingua predicate AST. Raises PredicateError for
    forms outside the supported vocabulary."""
    if isinstance(node, ast.BoolOp):
        op = "and" if isinstance(node.op, ast.And) else "or"
        return _fold(op, [predicate_from_py(v) for v in node.values])
    if isinstance(node, ast.UnaryOp):
        if isinstance(node.op, ast.Not):
            return p_app("not", [predicate_from_py(node.operand)])
        if isinstance(node.op, ast.USub):
            return p_app("neg", [predicate_from_py(node.operand)])
        raise PredicateError("unsupported unary operator")
    if isinstance(node, ast.Compare):
        terms, left = [], node.left
        for op, right in zip(node.ops, node.comparators):
            if type(op) not in _CMP:
                raise PredicateError("unsupported comparison operator")
            terms.append(p_app(_CMP[type(op)], [predicate_from_py(left), predicate_from_py(right)]))
            left = right
        return terms[0] if len(terms) == 1 else _fold("and", terms)
    if isinstance(node, ast.BinOp):
        if type(node.op) not in _BIN:
            raise PredicateError("unsupported arithmetic operator")
        return p_app(_BIN[type(node.op)], [predicate_from_py(node.left), predicate_from_py(node.right)])
    if isinstance(node, ast.Call):
        fn = node.func
        name = fn.id if isinstance(fn, ast.Name) else (fn.attr if isinstance(fn, ast.Attribute) else None)
        if name == "len" and len(node.args) == 1 and not node.keywords:
            return p_app("length", [predicate_from_py(node.args[0])])
        raise PredicateError(f"unsupported call {name!r}")
    if isinstance(node, ast.Name):
        return p_var(node.id)
    if isinstance(node, ast.Constant):
        if isinstance(node.value, (bool, int, float, str)) or node.value is None:
            return p_lit(node.value)
        raise PredicateError("unsupported literal")
    raise PredicateError(f"unsupported expression {type(node).__name__}")
