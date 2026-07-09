"""Pragmatic body-expression AST builder (spec/body-expression.schema.json).

The Python front-end (`body_ast_from_py`) translates a usefully-large pure subset to a real,
**executable** Nova Lingua body — a `lambda` over the function's parameters (the canonical runnable
form, matching `spec/examples/body-double.json`) whose body is built from:
  * expressions — `var` / `lit` / `app` / `field`, operators (`a + b`, `!x`, `a == b` → `add`/`not`/
    `eq`/…, the predicate-layer vocabulary), a few mapped Python builtins (`len` → `length`,
    `abs` → `abs`), and the conditional expression `a if c else b`;
  * local bindings — `x = expr; …; return r` → nested `let`;
  * conditionals — `if c: …` / `elif` / `else` and early `return` → `case` on the boolean test
    (the schema has no `if`; it is `case` on a `bool`);
  * list comprehensions — `[elt for v in src (if cond)]` → `map`/`filter` over `src`;
  * accumulator loops — `for x in src: acc = update` → `foldl(\acc x -> update, acc, src)`;
  * string idioms (spec/expressiveness.md phase 4) — driven by a *known-string* inference rooted in
    `str`-annotated parameters (plus str literals, `str(…)`, `.join(…)`, stringish `let`s):
    `a + b` → `str_concat`, `len(s)` → `str_length`, `s.split(sep)` → `str_split(sep, s)`
    (separator-first — receiver and argument swap), `sep.join(xs)` → `str_join`,
    `needle in s` → `str_contains`, `str(n)` → `to_string`, and **f-strings** —
    `f"n={n}"` → `str_concat("n=", to_string(n))` (known-string interpolations skip the
    `to_string`; `!r`-style conversions and format specs are out of subset). Unannotated code
    keeps its numeric/list reading, and a wrong guess fails the example gate rather than
    shipping wrong.
Conditionals (and comprehension filters) are only translated when the test is genuinely boolean (a
comparison / boolean connective / `not` / bool literal) so Python truthiness is never silently
mistranslated. Anything outside the subset (`while`, non-accumulator `for`, multi-generator / dict /
set comprehensions, `with`/`try`, truthy non-bool tests, unrepresentable sub-expressions) yields
None, and the adapter keeps its synthetic source-hash body — byte-identical to before. A
zero-parameter function emits the bare result expression (no `lambda`), so applying it to `[]` still
evaluates.

Front-ends:
  * `body_ast_from_py` — real Python `ast` (nl-ingest-py): the executable subset above.
  * `body_ast_from_hs` — string-scanner recognizer (a bare variable or a flat application of atoms,
    `f a b`), now `lambda`-wrapped over the equation's parameters so it is executable.
  * `body_ast_from_ts` — a TS arrow's expression body is parsed with Python's `ast` (TS expression
    syntax coincides with Python's for the supported subset — identifiers, literals, arithmetic /
    comparison operators, calls, member access) and reused via `_expr_from_py`, `lambda`-wrapped over
    the arrow's parameters. TS-only syntax (`?:`, `===`, `!`) and block bodies yield None.
The Rust adapter has its own parallel `body_ast` over `syn` in nl_ingest.rs.
"""

import ast
import re

from nl_core import split_top
from nl_values import ValueEncodeError, to_value_ast

_VAR = re.compile(r"^[a-z_][a-zA-Z0-9_']*$")
_FIELD = re.compile(r"^[a-z][a-zA-Z0-9_]*$")

# Python operator -> Nova builtin op name (shared with the predicate vocabulary). `//` maps to the
# same `div` as `/`: Nova div on ints is Euclidean, Python `//` floors — they agree whenever the
# divisor is positive (the common case) and a wrong guess fails the example gate, the same honesty
# contract the existing `%` -> floored-vs-Euclidean `mod` mapping already carries.
_PY_CMP = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq", ast.NotEq: "neq"}
_PY_BIN = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div", ast.Mod: "mod",
           ast.FloorDiv: "div"}
_PY_BOOL = {ast.And: "and", ast.Or: "or"}
# Python builtin call -> Nova builtin (unary, unambiguous; arity-ambiguous ones like min/max excluded).
# `str` (and TS's `String`) map to the canonical-decimal `to_string`; a semantic mismatch (e.g. str of
# a float) fails the example gate rather than shipping wrong.
_PY_CALL = {"len": "length", "abs": "abs", "str": "to_string", "String": "to_string"}


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


def b_let(name, value, body):
    if not _VAR.match(name):
        raise BodyError(f"{name!r} is not a valid let-binding name")
    return {"kind": "let", "name": name, "value": value, "body": body}


def b_lambda(params, body):
    for p in params:
        if not _VAR.match(p):
            raise BodyError(f"{p!r} is not a valid parameter name")
    return {"kind": "lambda", "params": [{"name": p} for p in params], "body": body}


# A `case` on a boolean test: `true -> then`, wildcard `-> else` (the schema has no `if`).
def b_if(test, then_expr, else_expr):
    return {
        "kind": "case",
        "scrutinee": test,
        "arms": [
            {"pattern": {"kind": "lit", "value": {"kind": "bool", "value": True}}, "body": then_expr},
            {"pattern": {"kind": "wildcard"}, "body": else_expr},
        ],
    }


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

def _is_stringish(node, strs, dicts=frozenset()):
    """Whether `node` is syntactically known to produce a STRING: a str literal, a str-annotated
    parameter (or a `let` name bound to a stringish value), `str(x)`, a `sep.join(...)` call, string
    concatenation, or a ternary whose branches are both stringish. Drives the type-dependent
    translations (`+` -> str_concat, `len` -> str_length, `in` -> str_contains) that a purely
    syntactic adapter cannot otherwise decide; anything unproven stays on the numeric/list reading,
    and a wrong guess fails the example gate rather than shipping wrong."""
    if isinstance(node, ast.Constant):
        return isinstance(node.value, str)
    if isinstance(node, ast.Name):
        return node.id in strs
    if isinstance(node, ast.Call):
        if isinstance(node.func, ast.Name) and node.func.id == "str":
            return True
        if isinstance(node.func, ast.Attribute) and node.func.attr == "join":
            return True
        return False
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _is_stringish(node.left, strs, dicts) or _is_stringish(node.right, strs, dicts)
    if isinstance(node, ast.IfExp):
        return _is_stringish(node.body, strs, dicts) and _is_stringish(node.orelse, strs, dicts)
    if isinstance(node, ast.JoinedStr):
        return True  # an f-string always produces a string
    return False


def _is_dictish(node, dicts):
    """Whether `node` is syntactically known to be a DICT (a `dict`-annotated parameter, or a `let`
    name bound to one). Drives the map-idiom translations (`d.get(k, v)`, `k in d`, `len(d)`,
    `sorted(d.keys())`); anything unproven keeps its untyped reading, and a wrong guess fails the
    example gate rather than shipping wrong."""
    return isinstance(node, ast.Name) and node.id in dicts


def _maybe_case(scrutinee, just_body_of, none_body):
    """`case <scrutinee> of { Just(_got) => <just_body_of(var)>; None => <none_body> }` — the shape
    every Maybe-consuming translation shares."""
    return {
        "kind": "case",
        "scrutinee": scrutinee,
        "arms": [
            {"pattern": {"kind": "variant", "tag": "Just", "payload": {"kind": "bind", "name": "_got"}},
             "body": just_body_of(b_var("_got"))},
            {"pattern": {"kind": "variant", "tag": "None"}, "body": none_body},
        ],
    }


def _is_boolish(node):
    """True if `node` is a genuinely boolean expression — so a Python `if`/ternary test can be a
    `case` on a `bool` without silently mistranslating truthiness of non-bool values."""
    if isinstance(node, ast.Compare):
        return True
    if isinstance(node, ast.Constant) and isinstance(node.value, bool):
        return True
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
        return _is_boolish(node.operand)
    if isinstance(node, ast.BoolOp):
        return all(_is_boolish(v) for v in node.values)
    return False


def _expr_from_py(node, strs=frozenset(), dicts=frozenset()):
    if isinstance(node, ast.JoinedStr):
        # f-strings (spec/expressiveness.md phase 4): f"n={n}" -> str_concat("n=", to_string(n)).
        # A known-string interpolation passes through as-is; anything else goes through to_string
        # (Python renders ints identically; a non-int mismatch fails the example gate rather than
        # shipping wrong). Conversions (!r/!s/!a) and format specs are out of subset.
        parts = []
        for piece in node.values:
            if isinstance(piece, ast.Constant) and isinstance(piece.value, str):
                if piece.value:
                    parts.append(b_lit(_value(piece.value)))
            elif isinstance(piece, ast.FormattedValue):
                if piece.conversion != -1 or piece.format_spec is not None:
                    raise BodyError("f-string conversions/format specs are out of subset")
                inner = _expr_from_py(piece.value, strs, dicts)
                parts.append(inner if _is_stringish(piece.value, strs, dicts)
                             else b_app(b_var("to_string"), [inner]))
            else:
                raise BodyError("unsupported f-string piece")
        if not parts:
            return b_lit(_value(""))
        expr = parts.pop()
        while parts:
            expr = b_app(b_var("str_concat"), [parts.pop(), expr])
        return expr
    if isinstance(node, ast.IfExp):
        if not _is_boolish(node.test):
            raise BodyError("non-boolean ternary test (Python truthiness is not representable)")
        return b_if(_expr_from_py(node.test, strs, dicts), _expr_from_py(node.body, strs, dicts),
                    _expr_from_py(node.orelse, strs, dicts))
    if isinstance(node, ast.ListComp):
        # [elt for v in src (if cond)] -> map(\v -> elt, filter(\v -> cond, src))  (builtins, no loop)
        if len(node.generators) != 1:
            raise BodyError("only single-generator comprehensions are in subset")
        gen = node.generators[0]
        if getattr(gen, "is_async", 0) or not isinstance(gen.target, ast.Name) or len(gen.ifs) > 1:
            raise BodyError("comprehension shape out of subset")
        var = gen.target.id
        inner = strs - {var}  # the comprehension variable shadows any annotated name
        inner_d = dicts - {var}
        src = _expr_from_py(gen.iter, strs, dicts)
        if gen.ifs:
            if not _is_boolish(gen.ifs[0]):
                raise BodyError("comprehension filter must be boolean")
            src = b_app(b_var("filter"), [b_lambda([var], _expr_from_py(gen.ifs[0], inner, inner_d)), src])
        elt = _expr_from_py(node.elt, inner, inner_d)
        if elt == b_var(var):
            return src  # `[v for v in src ...]` is the (filtered) source — no identity map
        return b_app(b_var("map"), [b_lambda([var], elt), src])
    if isinstance(node, ast.BoolOp):
        return _fold(_PY_BOOL[type(node.op)], [_expr_from_py(v, strs, dicts) for v in node.values])
    if isinstance(node, ast.UnaryOp):
        if isinstance(node.op, ast.Not):
            return _op_app("not", [_expr_from_py(node.operand, strs, dicts)])
        if isinstance(node.op, ast.USub):
            if isinstance(node.operand, ast.Constant) and isinstance(node.operand.value, (int, float)) \
                    and not isinstance(node.operand.value, bool):
                return b_lit(_value(-node.operand.value))
            return _op_app("neg", [_expr_from_py(node.operand, strs, dicts)])
        raise BodyError("unsupported unary operator")
    if isinstance(node, ast.Compare):
        terms, left = [], node.left
        for op, right in zip(node.ops, node.comparators):
            # `needle in s` over a KNOWN string -> str_contains (needle-first); `k in d` over a KNOWN
            # dict -> a has-key case over map_get's Maybe. Membership over anything unproven stays out
            # of subset (there is no list-membership builtin).
            if type(op) in (ast.In, ast.NotIn):
                if _is_stringish(right, strs):
                    t = b_app(b_var("str_contains"),
                              [_expr_from_py(left, strs, dicts), _expr_from_py(right, strs, dicts)])
                elif _is_dictish(right, dicts):
                    t = _maybe_case(
                        b_app(b_var("map_get"), [_expr_from_py(left, strs, dicts), _expr_from_py(right, strs, dicts)]),
                        lambda _v: b_lit(_value(True)), b_lit(_value(False)))
                else:
                    raise BodyError("`in` is only in subset over a known string or dict")
                terms.append(_op_app("not", [t]) if isinstance(op, ast.NotIn) else t)
            elif type(op) not in _PY_CMP:
                raise BodyError("unsupported comparison operator")
            else:
                terms.append(_op_app(_PY_CMP[type(op)], [_expr_from_py(left, strs, dicts), _expr_from_py(right, strs, dicts)]))
            left = right
        return terms[0] if len(terms) == 1 else _fold("and", terms)
    if isinstance(node, ast.BinOp):
        # String concatenation: `a + b` where either side is a known string -> str_concat.
        if isinstance(node.op, ast.Add) and (_is_stringish(node.left, strs, dicts) or _is_stringish(node.right, strs, dicts)):
            return b_app(b_var("str_concat"), [_expr_from_py(node.left, strs, dicts), _expr_from_py(node.right, strs, dicts)])
        if type(node.op) not in _PY_BIN:
            raise BodyError("unsupported arithmetic operator")
        return _op_app(_PY_BIN[type(node.op)], [_expr_from_py(node.left, strs, dicts), _expr_from_py(node.right, strs, dicts)])
    if isinstance(node, ast.Call):
        if node.keywords or any(isinstance(a, ast.Starred) for a in node.args):
            raise BodyError("calls with keyword/starred args are out of subset")
        fn = node.func
        # String-method idioms (spec/expressiveness.md phase 4): `s.split(sep)` and `sep.join(xs)`
        # map onto the separator-FIRST builtins (note split swaps receiver and argument). Only the
        # 1-argument `.split(sep)` translates — Python's 0-arg whitespace split has no counterpart.
        if isinstance(fn, ast.Attribute) and len(node.args) == 1:
            if fn.attr == "split" and _is_stringish(fn.value, strs, dicts):
                return b_app(b_var("str_split"),
                             [_expr_from_py(node.args[0], strs, dicts), _expr_from_py(fn.value, strs, dicts)])
            if fn.attr == "join":
                # Python order: `sep.join(xs)` (stringish receiver). TS/JS order: `xs.join(sep)`
                # (stringish ARGUMENT). Either way the separator goes first.
                if _is_stringish(fn.value, strs, dicts):
                    return b_app(b_var("str_join"),
                                 [_expr_from_py(fn.value, strs, dicts), _expr_from_py(node.args[0], strs, dicts)])
                if _is_stringish(node.args[0], strs, dicts):
                    return b_app(b_var("str_join"),
                                 [_expr_from_py(node.args[0], strs, dicts), _expr_from_py(fn.value, strs, dicts)])
            if fn.attr == "includes" and _is_stringish(fn.value, strs):
                # TS `s.includes(needle)` -> str_contains(needle, s) (needle-first builtin).
                return b_app(b_var("str_contains"),
                             [_expr_from_py(node.args[0], strs, dicts), _expr_from_py(fn.value, strs, dicts)])
        # Dict idioms (spec/expressiveness.md phase 4): only the TOTAL forms translate. The bare
        # 1-arg `d.get(k)` (an Optional at the record boundary — needs the None<->Maybe example
        # value-mapping design) stays out of subset; `d[k]` raises, so it stays out too.
        if isinstance(fn, ast.Attribute) and _is_dictish(fn.value, dicts):
            if fn.attr == "get" and len(node.args) == 2:
                # d.get(k, default) -> case map_get(k, d) of { Just(v) => v; None => default }.
                return _maybe_case(
                    b_app(b_var("map_get"), [_expr_from_py(node.args[0], strs, dicts),
                                             _expr_from_py(fn.value, strs, dicts)]),
                    lambda v: v, _expr_from_py(node.args[1], strs, dicts))
            if fn.attr == "get" and len(node.args) == 1:
                # The bare 1-arg get returns an Optional at the record boundary — out of subset
                # until the None<->Maybe example value-mapping is designed. Refuse explicitly
                # rather than emitting an unrunnable projection.
                raise BodyError("1-arg dict.get (Optional boundary) is out of subset")
        if isinstance(fn, ast.Name):
            # `len` of a known string is str_length (Unicode scalars — matches Python's len); of a
            # known dict, map_size.
            if fn.id == "len" and len(node.args) == 1 and _is_stringish(node.args[0], strs):
                return b_app(b_var("str_length"), [_expr_from_py(node.args[0], strs, dicts)])
            if fn.id == "len" and len(node.args) == 1 and _is_dictish(node.args[0], dicts):
                return b_app(b_var("map_size"), [_expr_from_py(node.args[0], strs, dicts)])
            # `sorted(d)` / `sorted(d.keys())` over a known dict -> map_keys (which is sorted —
            # deterministic iteration is exactly what makes this translation sound).
            if fn.id == "sorted" and len(node.args) == 1:
                target = node.args[0]
                if isinstance(target, ast.Call) and isinstance(target.func, ast.Attribute) \
                        and target.func.attr == "keys" and not target.args:
                    target = target.func.value
                if _is_dictish(target, dicts):
                    return b_app(b_var("map_keys"), [_expr_from_py(target, strs, dicts)])
            fnexpr = b_var(_PY_CALL.get(fn.id, fn.id))  # map len->length, abs->abs; else as-named
        elif isinstance(fn, ast.Attribute):
            fnexpr = _expr_from_py(fn, strs, dicts)  # qualified/method call -> app over a field projection
        else:
            raise BodyError("unsupported call target")
        return b_app(fnexpr, [_expr_from_py(a, strs, dicts) for a in node.args])
    if isinstance(node, ast.Attribute):
        return b_field(_expr_from_py(node.value, strs, dicts), node.attr)
    if isinstance(node, ast.Name):
        return b_var(node.id)
    if isinstance(node, ast.Constant):
        return b_lit(_value(node.value))
    if isinstance(node, ast.List):
        # A list literal: `[]` -> nil; `[a, b, …]` -> cons(a, cons(b, …, nil)). The empty case is
        # what makes an accumulator `result = []` before a build-loop expressible.
        if any(isinstance(e, ast.Starred) for e in node.elts):
            raise BodyError("starred list elements are out of subset")
        expr = b_var("nil")
        for e in reversed(node.elts):
            expr = b_app(b_var("cons"), [_expr_from_py(e, strs, dicts), expr])
        return expr
    raise BodyError(f"unsupported expression {type(node).__name__}")


def _block_from_py(stmts, strs=frozenset(), dicts=frozenset()):
    """Translate a statement sequence that must produce a value into an expression: `return r` is the
    result; `x = e; …` becomes `let x = e in …`; `if c: …`/`else`/early-return becomes `case`.
    `strs`/`dicts` carry the names known to hold STRINGS / DICTS (annotation-rooted, threaded through
    `let`s with shadowing)."""
    if not stmts:
        raise BodyError("block falls off the end without returning a value")
    head, tail = stmts[0], stmts[1:]
    if isinstance(head, ast.Return):
        if head.value is None:
            raise BodyError("bare `return` (no value)")
        return _expr_from_py(head.value, strs, dicts)  # statements after a return are dead
    if isinstance(head, ast.Assign):
        if len(head.targets) != 1 or not isinstance(head.targets[0], ast.Name):
            raise BodyError("only single-name assignment targets are in subset")
        name = head.targets[0].id
        inner = (strs | {name}) if _is_stringish(head.value, strs) else (strs - {name})
        inner_d = (dicts | {name}) if _is_dictish(head.value, dicts) else (dicts - {name})
        return b_let(name, _expr_from_py(head.value, strs, dicts), _block_from_py(tail, inner, inner_d))
    if isinstance(head, ast.AnnAssign):
        if not isinstance(head.target, ast.Name) or head.value is None:
            raise BodyError("annotated assignment must be `name: T = value`")
        name = head.target.id
        annotated_str = isinstance(head.annotation, ast.Name) and head.annotation.id == "str"
        annotated_dict = isinstance(head.annotation, ast.Name) and head.annotation.id in _DICT_ANNOTATIONS
        inner = (strs | {name}) if (annotated_str or _is_stringish(head.value, strs)) else (strs - {name})
        inner_d = (dicts | {name}) if (annotated_dict or _is_dictish(head.value, dicts)) else (dicts - {name})
        return b_let(name, _expr_from_py(head.value, strs, dicts), _block_from_py(tail, inner, inner_d))
    if isinstance(head, ast.AugAssign):
        # `acc += e` (or -=, *=, /=, %=) re-binds `acc` to `acc <op> e` — a `let` over the rest. `acc`
        # must already be bound (a parameter or a preceding assignment). `s += t` over a known string
        # is str_concat, like binary `+`.
        if not isinstance(head.target, ast.Name):
            raise BodyError("augmented assignment must target a single name")
        name = head.target.id
        if isinstance(head.op, ast.Add) and (name in strs or _is_stringish(head.value, strs)):
            update = b_app(b_var("str_concat"), [b_var(name), _expr_from_py(head.value, strs, dicts)])
            return b_let(name, update, _block_from_py(tail, strs | {name}, dicts - {name}))
        if type(head.op) not in _PY_BIN:
            raise BodyError("unsupported augmented-assignment operator")
        update = _op_app(_PY_BIN[type(head.op)], [b_var(name), _expr_from_py(head.value, strs, dicts)])
        return b_let(name, update, _block_from_py(tail, strs - {name}, dicts - {name}))
    if isinstance(head, ast.If):
        if not _is_boolish(head.test):
            raise BodyError("non-boolean `if` test (Python truthiness is not representable)")
        then_expr = _block_from_py(head.body, strs, dicts)
        # An `else`/`elif` block is the false branch; without one, the rest of the function is.
        else_expr = _block_from_py(head.orelse if head.orelse else tail, strs, dicts)
        return b_if(_expr_from_py(head.test, strs, dicts), then_expr, else_expr)
    if isinstance(head, ast.For):
        # A loop over `src`, its body optionally wrapped in one guard `if cond: …` (no else).
        # Four shapes:
        #   acc = update / acc <op>= e   -> acc = foldl(\acc x -> update, acc, src)   (a guard makes
        #                                    the step `case cond of true => update; false => acc`;
        #                                    SEVERAL such statements -> one fold per accumulator,
        #                                    exact only when independent)
        #   acc.append(e)                -> acc = append(acc, map(\x -> e, [filter(\x->cond,] src)))
        #   for i in inner: acc.append(e)-> nested list-building: a foldl of per-row appends
        #   return e                     -> early-return search: head of the filtered sublist, with
        #                                    the statements after the loop as the not-found branch
        # The accumulator shapes re-bind an `acc` that must already be bound (a preceding
        # `acc = init` -> let).
        if head.orelse or not isinstance(head.target, ast.Name):
            raise BodyError("only `for <name> in <src>:` accumulator loops are in subset")
        x = head.target.id
        # After the loop Python leaves x bound to the last element; none of the translations do,
        # so a tail that reads the loop variable is out of subset rather than silently wrong.
        for s in tail:
            if any(isinstance(n, ast.Name) and n.id == x and isinstance(n.ctx, ast.Load)
                   for n in ast.walk(s)):
                raise BodyError("loop variable read after a loop")
        loop_strs, loop_dicts = strs - {x}, dicts - {x}
        src = _expr_from_py(head.iter, strs, dicts)

        # Peel one optional guard `if cond: <body>` (no else) off the loop body.
        body = head.body
        guard = None
        if len(body) == 1 and isinstance(body[0], ast.If) and not body[0].orelse:
            if not _is_boolish(body[0].test):
                raise BodyError("non-boolean loop guard (Python truthiness is not representable)")
            guard = body[0].test
            body = body[0].body
        stmt = body[0] if len(body) == 1 else None

        # Shape: early-return search `for x in src: if cond: return e` with the block after the
        # loop as the not-found default. A fold can't short-circuit, but in a pure total language
        # the short-circuit is unobservable — find-first IS `head` of the guarded sublist:
        #   let hits = filter(\x -> cond, src) in
        #     case null(hits) of true => <rest of the function>; false => let x = head(hits) in e
        if isinstance(stmt, ast.Return):
            if stmt.value is None:
                raise BodyError("bare `return` in a loop (no value)")
            hits_src = src
            if guard is not None:
                hits_src = b_app(b_var("filter"),
                                 [b_lambda([x], _expr_from_py(guard, loop_strs, loop_dicts)), hits_src])
            found = _expr_from_py(stmt.value, loop_strs, loop_dicts)
            used = {x} | {n.id for s in stmts for n in ast.walk(s) if isinstance(n, ast.Name)}
            hits = "hits"
            while hits in used:
                hits += "_"
            hit = b_app(b_var("head"), [b_var(hits)])
            found_branch = hit if found == b_var(x) else b_let(x, hit, found)
            return b_let(hits, hits_src,
                         b_if(b_app(b_var("null"), [b_var(hits)]),
                              _block_from_py(tail, strs, dicts),
                              found_branch))

        # Shape: list-building via `acc.append(e)`.
        if isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Call) \
                and isinstance(stmt.value.func, ast.Attribute) and stmt.value.func.attr == "append" \
                and isinstance(stmt.value.func.value, ast.Name) and len(stmt.value.args) == 1 \
                and not stmt.value.keywords:
            acc = stmt.value.func.value.id
            src2 = src
            if guard is not None:
                src2 = b_app(b_var("filter"),
                             [b_lambda([x], _expr_from_py(guard, loop_strs, loop_dicts)), src2])
            elt = _expr_from_py(stmt.value.args[0], loop_strs, loop_dicts)
            mapped = src2 if elt == b_var(x) else b_app(b_var("map"), [b_lambda([x], elt), src2])
            # append onto the accumulator's prior value; `append(nil, L) = L`, so a `[]`-seeded
            # build is just the mapped/filtered list, and a non-empty seed is honored.
            rebind = b_app(b_var("append"), [b_var(acc), mapped])
            return b_let(acc, rebind, _block_from_py(tail, strs - {acc}, dicts - {acc}))

        # Shape: nested list-building loop `for x in xss: for i in <inner(x)>: acc.append(e)` —
        # flatten / flatMap. The sequential appends are a left fold over the outer source, each
        # step appending that row's (mapped/filtered) batch onto the accumulator so far:
        #   acc = foldl(\acc x -> append(acc, map(\i -> e, [filter(\i -> ic,] inner)), acc, xss)
        # (outer guard -> filter over xss; seeded with acc's prior value, so `[]` collapses as
        # above). The element/guards reading the accumulator mid-loop is refused — a fold step
        # sees only its own batch's `acc`, not Python's growing list.
        if isinstance(stmt, ast.For):
            if stmt.orelse or not isinstance(stmt.target, ast.Name):
                raise BodyError("only `for <name> in <src>:` loops are in subset")
            i = stmt.target.id
            in_strs, in_dicts = loop_strs - {i}, loop_dicts - {i}
            inner_src = _expr_from_py(stmt.iter, loop_strs, loop_dicts)
            ibody = stmt.body
            iguard = None
            if len(ibody) == 1 and isinstance(ibody[0], ast.If) and not ibody[0].orelse:
                if not _is_boolish(ibody[0].test):
                    raise BodyError("non-boolean loop guard (Python truthiness is not representable)")
                iguard = ibody[0].test
                ibody = ibody[0].body
            if not (len(ibody) == 1 and isinstance(ibody[0], ast.Expr)
                    and isinstance(ibody[0].value, ast.Call)
                    and isinstance(ibody[0].value.func, ast.Attribute)
                    and ibody[0].value.func.attr == "append"
                    and isinstance(ibody[0].value.func.value, ast.Name)
                    and len(ibody[0].value.args) == 1 and not ibody[0].value.keywords):
                raise BodyError("nested loop body must be a single (optionally guarded) `.append(…)`")
            receiver = ibody[0].value.func.value
            acc = receiver.id
            for n in ast.walk(head):
                if isinstance(n, ast.Name) and n.id == acc and isinstance(n.ctx, ast.Load) \
                        and n is not receiver:
                    raise BodyError("nested loop reads its accumulator mid-loop")
            batch = inner_src
            if iguard is not None:
                batch = b_app(b_var("filter"),
                              [b_lambda([i], _expr_from_py(iguard, in_strs, in_dicts)), batch])
            elt = _expr_from_py(ibody[0].value.args[0], in_strs, in_dicts)
            if elt != b_var(i):
                batch = b_app(b_var("map"), [b_lambda([i], elt), batch])
            outer_src = src
            if guard is not None:
                outer_src = b_app(b_var("filter"),
                                  [b_lambda([x], _expr_from_py(guard, loop_strs, loop_dicts)), outer_src])
            step = b_lambda([acc, x], b_app(b_var("append"), [b_var(acc), batch]))
            fold = b_app(b_var("foldl"), [step, b_var(acc), outer_src])
            return b_let(acc, fold, _block_from_py(tail, strs - {acc}, dicts - {acc}))

        # Shape: numeric/string/structural accumulator statements `acc = update` / `acc <op>= e` —
        # one, or SEVERAL over distinct accumulators. Each becomes its own foldl over `src`;
        # splitting one pass into N is exact only when the statements are independent (an update or
        # the guard reading ANOTHER accumulator sees a mid-loop value a separate fold can't
        # reproduce), so dependence is refused rather than silently mistranslated. Re-walking the
        # list N times is unobservable in a pure total language, like the search loop's skipped
        # short-circuit.
        if body and all(isinstance(s, (ast.Assign, ast.AugAssign)) for s in body):
            accs, updates = [], []
            for stmt in body:
                if isinstance(stmt, ast.AugAssign):
                    if not isinstance(stmt.target, ast.Name):
                        raise BodyError("accumulator assignment must target a single name")
                    acc = stmt.target.id
                    if isinstance(stmt.op, ast.Add) and (acc in strs or _is_stringish(stmt.value, loop_strs)):
                        update = b_app(b_var("str_concat"), [b_var(acc), _expr_from_py(stmt.value, loop_strs, loop_dicts)])
                    elif type(stmt.op) not in _PY_BIN:
                        raise BodyError("unsupported augmented-assignment operator")
                    else:
                        update = _op_app(_PY_BIN[type(stmt.op)], [b_var(acc), _expr_from_py(stmt.value, loop_strs, loop_dicts)])
                else:
                    if len(stmt.targets) != 1 or not isinstance(stmt.targets[0], ast.Name):
                        raise BodyError("accumulator assignment must target a single name")
                    acc = stmt.targets[0].id
                    update = _expr_from_py(stmt.value, loop_strs, loop_dicts)
                accs.append(acc)
                updates.append(update)
            if len(accs) > 1:
                if len(set(accs)) != len(accs):
                    raise BodyError("duplicate accumulator in a multi-accumulator loop")
                for stmt in body:
                    target = stmt.target if isinstance(stmt, ast.AugAssign) else stmt.targets[0]
                    others = set(accs) - {target.id}
                    if any(isinstance(n, ast.Name) and n.id in others for n in ast.walk(stmt.value)):
                        raise BodyError("sequentially dependent accumulators (an update reads another)")
                if guard is not None \
                        and any(isinstance(n, ast.Name) and n.id in set(accs) for n in ast.walk(guard)):
                    raise BodyError("loop guard reads an accumulator in a multi-accumulator loop")
            result = _block_from_py(tail, strs, dicts)
            for acc, update in reversed(list(zip(accs, updates))):
                # A guarded step keeps the accumulator unchanged on the false branch.
                if guard is not None:
                    update = b_if(_expr_from_py(guard, loop_strs, loop_dicts), update, b_var(acc))
                fold = b_app(b_var("foldl"), [b_lambda([acc, x], update), b_var(acc), src])
                result = b_let(acc, fold, result)
            return result
        raise BodyError("loop body must be an accumulator assignment or an `.append(…)`")
    raise BodyError(f"unsupported statement {type(head).__name__}")


def _fixed_param_names(func):
    a = func.args
    return [p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))]


_DICT_ANNOTATIONS = ("dict", "Dict", "Mapping", "MutableMapping")


def _str_annotated_params(func):
    """The parameter names annotated `str` — the roots of the known-string inference that drives the
    type-dependent translations (`+` -> str_concat, `len` -> str_length, `.split`/`.join`, `in`)."""
    a = func.args
    return frozenset(p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))
                     if isinstance(p.annotation, ast.Name) and p.annotation.id == "str")


def _dict_annotated_params(func):
    """The parameter names annotated as dicts (`dict`, `dict[...]`, `Dict[...]`, `Mapping[...]`) —
    the roots of the known-dict inference behind the map-idiom translations."""
    def is_dict_ann(ann):
        if isinstance(ann, ast.Name):
            return ann.id in _DICT_ANNOTATIONS
        if isinstance(ann, ast.Subscript) and isinstance(ann.value, ast.Name):
            return ann.value.id in _DICT_ANNOTATIONS
        return False
    a = func.args
    return frozenset(p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))
                     if is_dict_ann(p.annotation))


def body_ast_from_py(func):
    """An *executable* body AST for a Python function whose body is in the supported subset (a
    `lambda` over its parameters; a bare expression for a 0-parameter function), or None otherwise."""
    body = func.body
    start = 1 if (body and isinstance(body[0], ast.Expr)
                  and isinstance(body[0].value, ast.Constant)
                  and isinstance(body[0].value.value, str)) else 0
    stmts = body[start:]
    if not stmts:
        return None
    try:
        expr = _block_from_py(stmts, _str_annotated_params(func), _dict_annotated_params(func))
        params = _fixed_param_names(func)
        # A 0-arg function is its bare result (applying it to [] still evaluates); else wrap in a
        # lambda so `run` can apply the example's arguments.
        return expr if not params else b_lambda(params, expr)
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
    """An *executable* body AST (a `lambda` over the equation's parameters) for a single-clause Haskell
    equation whose RHS is a bare variable or a flat application of atoms (`f a b`). Guards, operators,
    multi-line / multi-clause bodies -> None."""
    if not equation_text or "\n" in equation_text:
        return None
    text = equation_text.strip()
    if "|" in text:  # guards
        return None
    parts = split_top(text, "=")  # splits on EVERY top-level '=' so any ==/<=/>= in the RHS rejects
    if len(parts) != 2:
        return None
    lhs, rhs = parts[0].strip(), parts[1].strip()
    lhs_toks = lhs.split()
    if not lhs_toks or lhs_toks[0] != name:
        return None
    params = lhs_toks[1:]  # the equation's parameters, e.g. `f x y = …` -> [x, y]
    try:
        toks = [t for t in split_top(rhs, " ") if t.strip()]
        if not toks:
            return None
        if len(toks) == 1:
            expr = _atom(toks[0])
        else:
            fn = _atom(toks[0])
            if fn["kind"] != "var":
                return None
            expr = b_app(fn, [_atom(t) for t in toks[1:]])
        return b_lambda(params, expr) if params else expr
    except BodyError:
        return None


def _ts_params(prefix):
    """Parameter names of a TS arrow from the text before `=>` — `(x, y)` / `(x: T): R` / bare `x`.
    A trailing return-type annotation (`): R`) is ignored; per-parameter type annotations and defaults
    are stripped. None if a parameter isn't a simple identifier."""
    p = prefix.strip()
    rp = p.rfind(")")  # close of the parameter list (a `): R` return type may follow it)
    if rp != -1:
        depth, start = 0, None
        for i in range(rp, -1, -1):
            if p[i] == ")":
                depth += 1
            elif p[i] == "(":
                depth -= 1
                if depth == 0:
                    start = i
                    break
        if start is None:
            return None
        inner = p[start + 1:rp].strip()
        if not inner:
            return []
        names = []
        for part in split_top(inner, ","):
            nm = part.split(":")[0].split("=")[0].strip()
            if not _VAR.match(nm):
                return None
            names.append(nm)
        return names
    last = p.split()[-1] if p.split() else ""
    return [last] if _VAR.match(last) else None


def _ts_string_params(prefix):
    """The subset of a TS arrow's parameters annotated `: string` — the roots of the known-string
    inference, so `(s: string) => s.split(",")` lifts onto the string builtins like an annotated
    Python function does."""
    p = prefix.strip()
    rp = p.rfind(")")
    if rp == -1:
        return frozenset()
    depth, start = 0, None
    for i in range(rp, -1, -1):
        if p[i] == ")":
            depth += 1
        elif p[i] == "(":
            depth -= 1
            if depth == 0:
                start = i
                break
    if start is None:
        return frozenset()
    out = set()
    for part in split_top(p[start + 1:rp], ","):
        pieces = part.split(":")
        if len(pieces) >= 2:
            nm = pieces[0].strip()
            ty = pieces[1].split("=")[0].strip()
            if _VAR.match(nm) and ty == "string":
                out.add(nm)
    return frozenset(out)


def body_ast_from_ts(name, slice_text):
    """An *executable* body AST (a `lambda` over the arrow's parameters) for a TypeScript arrow with an
    *expression* body (`(x) => expr`). TypeScript expression syntax coincides with Python's for the
    supported subset (identifiers, literals, arithmetic / comparison operators, calls, member access,
    parens), so the body is parsed with Python's `ast` and reused via `_expr_from_py`. Block bodies,
    and TS-only syntax (`?:`, `===`, `!`) that doesn't parse as a Python expression, -> None."""
    if not slice_text:
        return None
    idx = slice_text.find("=>")
    if idx < 0:
        return None
    rhs = slice_text[idx + 2:].strip().rstrip(";").strip()
    if not rhs or rhs.startswith("{"):
        return None
    params = _ts_params(slice_text[:idx])
    if params is None:
        return None
    try:
        node = ast.parse(rhs, mode="eval").body
        expr = _expr_from_py(node, _ts_string_params(slice_text[:idx]))
    except (SyntaxError, ValueError, BodyError):
        return None
    return b_lambda(params, expr) if params else expr
