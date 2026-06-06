"""Pragmatic body-expression AST builder (spec/body-expression.schema.json).

v1 SUBSET — only a *single result expression* built exclusively from `var` / `lit` / `app` /
`field` is translated to a real Nova Lingua body AST. Anything with control flow, local bindings,
lambdas, pattern matching, comprehensions, multiple statements/equations, or any unrepresentable
sub-expression yields None, and the adapter keeps its synthetic source-hash body (byte-identical to
before). This mirrors the adapters' existing "fall back to v0.1 when there are no examples" pattern.

Parameters appear as FREE `var`s — the schema explicitly sanctions this ("the function's parameter
binding is on the OUTSIDE"); we emit no wrapping `lambda`. Operators (`a + b`, `!x`, `a == b`)
become an `app` whose `fn` is a `var` naming the builtin (add / sub / mul / div / mod / neg / not /
and / or / eq / neq / lt / le / gt / ge) — the same operator vocabulary the predicate layer uses.

Front-ends:
  * `body_ast_from_py` — real Python `ast` (nl-ingest-py); full subset support.
  * `body_ast_from_hs` / `body_ast_from_ts` — conservative recognizers for the string-scanner
    adapters, handling only a bare variable or a flat application of atoms (no operators); they
    almost always return None, which is the documented expected behaviour.
The Rust adapter has its own parallel `body_ast` over `syn` in nl_ingest.rs.
"""

import ast
import re

from nl_core import split_top
from nl_values import ValueEncodeError, to_value_ast

_VAR = re.compile(r"^[a-z_][a-zA-Z0-9_']*$")
_FIELD = re.compile(r"^[a-z][a-zA-Z0-9_]*$")

# Python operator -> Nova builtin op name (shared with the predicate vocabulary).
_PY_CMP = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq", ast.NotEq: "neq"}
_PY_BIN = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div", ast.Mod: "mod"}
_PY_BOOL = {ast.And: "and", ast.Or: "or"}


class BodyError(Exception):
    pass


# --- neutral constructors -----------------------------------------------------------------------

def b_var(name):
    if not _VAR.match(name):
        raise BodyError(f"{name!r} is not a valid body variable name")
    return {"kind": "var", "name": name}


def b_lit(value_ast):
    return {"kind": "lit", "value": value_ast}


def b_app(fn, args):
    return {"kind": "app", "fn": fn, "args": args}


def b_field(record, name):
    if not _FIELD.match(name):
        raise BodyError(f"{name!r} is not a valid field name")
    return {"kind": "field", "record": record, "name": name}


def _op_app(op, args):
    return b_app(b_var(op), args)


def _value(pyval):
    try:
        return to_value_ast(pyval)
    except ValueEncodeError as e:
        raise BodyError(str(e))


def _fold(op, terms):
    """Right-fold >=1 body terms into nested binary `app(var(op), [a, b])`."""
    acc = terms[-1]
    for t in reversed(terms[:-1]):
        acc = _op_app(op, [t, acc])
    return acc


# --- Python front-end ---------------------------------------------------------------------------

def _expr_from_py(node):
    if isinstance(node, ast.BoolOp):
        return _fold(_PY_BOOL[type(node.op)], [_expr_from_py(v) for v in node.values])
    if isinstance(node, ast.UnaryOp):
        if isinstance(node.op, ast.Not):
            return _op_app("not", [_expr_from_py(node.operand)])
        if isinstance(node.op, ast.USub):
            if isinstance(node.operand, ast.Constant) and isinstance(node.operand.value, (int, float)) \
                    and not isinstance(node.operand.value, bool):
                return b_lit(_value(-node.operand.value))
            return _op_app("neg", [_expr_from_py(node.operand)])
        raise BodyError("unsupported unary operator")
    if isinstance(node, ast.Compare):
        terms, left = [], node.left
        for op, right in zip(node.ops, node.comparators):
            if type(op) not in _PY_CMP:
                raise BodyError("unsupported comparison operator")
            terms.append(_op_app(_PY_CMP[type(op)], [_expr_from_py(left), _expr_from_py(right)]))
            left = right
        return terms[0] if len(terms) == 1 else _fold("and", terms)
    if isinstance(node, ast.BinOp):
        if type(node.op) not in _PY_BIN:
            raise BodyError("unsupported arithmetic operator")
        return _op_app(_PY_BIN[type(node.op)], [_expr_from_py(node.left), _expr_from_py(node.right)])
    if isinstance(node, ast.Call):
        if node.keywords or any(isinstance(a, ast.Starred) for a in node.args):
            raise BodyError("calls with keyword/starred args are out of subset")
        fn = node.func
        if isinstance(fn, ast.Name):
            fnexpr = b_var(fn.id)
        elif isinstance(fn, ast.Attribute):
            fnexpr = _expr_from_py(fn)  # qualified/method call -> app over a field projection
        else:
            raise BodyError("unsupported call target")
        return b_app(fnexpr, [_expr_from_py(a) for a in node.args])
    if isinstance(node, ast.Attribute):
        return b_field(_expr_from_py(node.value), node.attr)
    if isinstance(node, ast.Name):
        return b_var(node.id)
    if isinstance(node, ast.Constant):
        return b_lit(_value(node.value))
    raise BodyError(f"unsupported expression {type(node).__name__}")


def body_ast_from_py(func):
    """A body AST for a single-`return`-expression Python function (after an optional docstring), or
    None if the body falls outside the v1 subset."""
    body = func.body
    start = 1 if (body and isinstance(body[0], ast.Expr)
                  and isinstance(body[0].value, ast.Constant)
                  and isinstance(body[0].value.value, str)) else 0
    stmts = body[start:]
    if len(stmts) != 1 or not isinstance(stmts[0], ast.Return) or stmts[0].value is None:
        return None
    try:
        return _expr_from_py(stmts[0].value)
    except BodyError:
        return None


# --- atom / flat-application recognizer (Haskell & TypeScript string scanners) ------------------

def _atom(tok):
    """A body expr for a single token: a `var`, an int/float `lit`, or a string `lit`."""
    tok = tok.strip()
    if not tok:
        raise BodyError("empty atom")
    if _VAR.match(tok):
        return b_var(tok)
    if re.fullmatch(r"-?\d+", tok):
        return b_lit(_value(int(tok)))
    if re.fullmatch(r"-?\d+\.\d+", tok):
        return b_lit(_value(float(tok)))
    if len(tok) >= 2 and tok[0] in "\"'" and tok[-1] == tok[0]:
        return b_lit(_value(tok[1:-1]))
    raise BodyError(f"{tok!r} is not a simple atom")


def body_ast_from_hs(name, equation_text):
    """A body AST for a single-clause Haskell equation whose RHS is a bare variable or a flat
    application of atoms (`f a b`). Guards, operators, multi-line / multi-clause bodies -> None."""
    if not equation_text or "\n" in equation_text:
        return None
    text = equation_text.strip()
    if "|" in text:  # guards
        return None
    parts = split_top(text, "=")  # splits on EVERY top-level '=' so any ==/<=/>= in the RHS rejects
    if len(parts) != 2:
        return None
    lhs, rhs = parts[0].strip(), parts[1].strip()
    if not lhs.split() or lhs.split()[0] != name:
        return None
    try:
        toks = [t for t in split_top(rhs, " ") if t.strip()]
        if not toks:
            return None
        if len(toks) == 1:
            return _atom(toks[0])
        fn = _atom(toks[0])
        if fn["kind"] != "var":
            return None
        return b_app(fn, [_atom(t) for t in toks[1:]])
    except BodyError:
        return None


def body_ast_from_ts(name, slice_text):
    """A body AST for a TypeScript arrow function with an *expression* body (`(x) => expr`) where
    expr is a bare identifier or a flat call `g(a, b)`. Block bodies / operators -> None."""
    if not slice_text:
        return None
    idx = slice_text.find("=>")
    if idx < 0:
        return None
    rhs = slice_text[idx + 2:].strip().rstrip(";").strip()
    if not rhs or rhs.startswith("{"):
        return None
    try:
        if _VAR.match(rhs):
            return b_var(rhs)
        m = re.match(r"^([A-Za-z_$][\w$]*)\((.*)\)$", rhs, re.S)
        if not m:
            return _atom(rhs)
        callee, inner = m.group(1), m.group(2).strip()
        if not _VAR.match(callee):
            return None
        fn = b_var(callee)
        if inner == "":
            return b_app(fn, [])
        return b_app(fn, [_atom(a) for a in split_top(inner, ",")])
    except BodyError:
        return None
