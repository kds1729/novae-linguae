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
  * subscripts (2026-07-13, the survey's last design frontiers) — READS ride the raise-totalization
    boundary (the function's result Maybe-wraps): `d[k]` → `map_get`, `xs[i]` → the canonical `nth`
    commons record by `fn_ref` (see `nl_canon`), the idiomatic `xs[-1]` → a null-guarded `last`;
    STORES are total: `d[k] = v` → the `map_put` rebind;
  * counted iteration — `for i in range(a, b)` and the **counting `while`** (`while i < n: …;
    i += 1`, plus `<=` and the descending `>`/`>=` with `i -= 1`) desugar to the ordinary loop
    shapes over the canonical `range` record applied by `fn_ref`.
Conditionals (and comprehension filters) are only translated when the test is genuinely boolean (a
comparison / boolean connective / `not` / bool literal) so Python truthiness is never silently
mistranslated. Anything outside the subset (non-counting `while`, non-accumulator `for`,
multi-generator / dict / set comprehensions, `with`/`try`, truthy non-bool tests, unrepresentable
sub-expressions) yields None, and the adapter keeps its synthetic source-hash body — byte-identical
to before. A zero-parameter function emits the bare result expression (no `lambda`), so applying it
to `[]` still evaluates.

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

from nl_canon import NTH_HASH, RANGE_HASH
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


def b_variant(tag, payload=None):
    v = {"kind": "variant", "tag": tag}
    if payload is not None:
        v["payload"] = payload
    return v


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


def _is_listish(node, lists):
    """Whether `node` is syntactically known to be a LIST: a list-annotated parameter (or a `let`
    name threaded through `lists`), or a list literal. Roots the subscript translation — like the
    dict inference, only a PROVEN type may translate."""
    if isinstance(node, ast.Name):
        return node.id in lists
    return isinstance(node, ast.List)


def _fn_ref_app(target, args):
    """Apply a canonical commons record BY CONTENT-ADDRESS — an applied `fn_ref` literal. The
    adapter must ship the referenced record + body alongside the emitted one
    (`nl_canon.canonical_dependency_artifacts`) so `run --records` links it."""
    return b_app({"kind": "lit", "value": {"kind": "fn_ref", "target": target}}, args)


def _subscript_read(node, strs, dicts, lists):
    """The MAYBE-valued translation of a read subscript, or None when the root's type is unproven
    or the shape is out of subset:
        d[k]    (known dict)          -> map_get(k, d)
        xs[i]   (known list)          -> nth(i, xs)      (the canonical commons record, by fn_ref)
        xs[-1]  (known list, literal) -> case null(xs) of { true => None; false => Just(last(xs)) }
    Partiality is the point: Python's raising access becomes a Maybe, and the caller (a
    `let`/`return` in a Maybe-totalized function) supplies the None short-circuit — the same
    boundary raise-totalization draws. A NEGATIVE non-literal index deliberately diverges from
    Python's tail-indexing (`nth` answers None) — annotations cannot prove sign, the idiomatic
    literal `xs[-1]` translates exactly, and a wrong guess fails the example gate rather than
    shipping wrong (the `//` -> `div` honesty contract). Other negative literals stay out."""
    if not isinstance(node, ast.Subscript):
        return None
    root, idx = node.value, node.slice
    if isinstance(idx, ast.Slice):
        return None
    if _is_dictish(root, dicts):
        return b_app(b_var("map_get"),
                     [_expr_from_py(idx, strs, dicts), _expr_from_py(root, strs, dicts)])
    if _is_listish(root, lists):
        xs = _expr_from_py(root, strs, dicts)
        if isinstance(idx, ast.UnaryOp) and isinstance(idx.op, ast.USub) \
                and isinstance(idx.operand, ast.Constant):
            if idx.operand.value == 1:
                return b_if(b_app(b_var("null"), [xs]), b_variant("None"),
                            b_variant("Just", b_app(b_var("last"), [xs])))
            return None  # xs[-2]…: not the idiom, out of subset
        return _fn_ref_app(NTH_HASH, [_expr_from_py(idx, strs, dicts), xs])
    return None


def _is_maybeish(node, maybes, dicts):
    """Whether `node` is syntactically known to produce a MAYBE: an `Optional`-annotated parameter
    (or a `let` name bound to one), or the bare 1-arg `d.get(k)` over a known dict (map_get's
    Maybe). Drives the None<->Maybe boundary translations — narrowing `if x is None:` and the
    Just-wrapping of returns in an `-> Optional[T]` function."""
    if isinstance(node, ast.Name):
        return node.id in maybes
    return (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr == "get" and len(node.args) == 1 and not node.keywords
            and _is_dictish(node.func.value, dicts))


def _none_test(test, maybes):
    """`(name, narrows_on_none)` when `test` is `x is None` / `x is not None` over a known-Maybe
    name — the Python narrowing idiom that becomes a `case` on the Maybe — else None."""
    if isinstance(test, ast.Compare) and len(test.ops) == 1 \
            and type(test.ops[0]) in (ast.Is, ast.IsNot) \
            and isinstance(test.comparators[0], ast.Constant) and test.comparators[0].value is None \
            and isinstance(test.left, ast.Name) and test.left.id in maybes:
        return test.left.id, isinstance(test.ops[0], ast.Is)
    return None


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


def _tuple_target_names(target):
    """The element names of a tuple/list assignment or for-loop target (`x, y` or `[x, y]`) — a
    flat sequence of plain names, >=2. Nested, starred, or non-name elements are out of subset."""
    elts = target.elts
    if len(elts) < 2:
        raise BodyError("a tuple target needs at least two names")
    names = []
    for e in elts:
        if not isinstance(e, ast.Name):
            raise BodyError("tuple-unpacking targets must be plain names (no nesting/starred)")
        names.append(e.id)
    if len(set(names)) != len(names):
        raise BodyError("repeated name in a tuple-unpacking target")
    return names


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


def _test_from_py(test, strs, dicts, maybes, ints, lists):
    """A Python TEST position (an `if`-statement test or a loop guard) as a boolean body expression.
    A genuinely boolean test translates as before; non-bool TRUTHINESS desugars only where an
    annotation PROVES the type — the falsy set is type-dependent, so guessing is mistranslation:
        str -> x != ""    int -> x != 0    list -> not (null x)    dict -> map_size x != 0
    `not` and `and`/`or` recurse over mixed truthy/boolean operands (strict connectives instead of
    Python's short-circuit — unobservable in a pure total language, the established purity
    argument). OPTIONAL truthiness is REFUSED: `if x:` over a Maybe conflates None with a falsy
    payload — the `is None` narrowing idiom is the precise, already-supported form. Anything
    unproven refuses rather than shipping a wrong falsy set."""
    if _is_boolish(test):
        return _expr_from_py(test, strs, dicts)
    if isinstance(test, ast.UnaryOp) and isinstance(test.op, ast.Not):
        return _op_app("not", [_test_from_py(test.operand, strs, dicts, maybes, ints, lists)])
    if isinstance(test, ast.BoolOp):
        return _fold(_PY_BOOL[type(test.op)],
                     [_test_from_py(v, strs, dicts, maybes, ints, lists) for v in test.values])
    if isinstance(test, ast.Name):
        if test.id in strs:
            return _op_app("neq", [b_var(test.id), b_lit(_value(""))])
        if test.id in ints:
            return _op_app("neq", [b_var(test.id), b_lit(_value(0))])
        if test.id in lists:
            return _op_app("not", [b_app(b_var("null"), [b_var(test.id)])])
        if test.id in dicts:
            return _op_app("neq", [b_app(b_var("map_size"), [b_var(test.id)]), b_lit(_value(0))])
        if test.id in maybes:
            raise BodyError("Optional truthiness conflates None with a falsy payload — "
                            "narrow with `is None` / `is not None` instead")
    raise BodyError("non-boolean test (truthiness needs an annotation-proven str/int/list/dict)")


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
        # `d[k]` raises, so it stays out of subset.
        if isinstance(fn, ast.Attribute) and _is_dictish(fn.value, dicts):
            if fn.attr == "get" and len(node.args) == 2:
                # d.get(k, default) -> case map_get(k, d) of { Just(v) => v; None => default }.
                return _maybe_case(
                    b_app(b_var("map_get"), [_expr_from_py(node.args[0], strs, dicts),
                                             _expr_from_py(fn.value, strs, dicts)]),
                    lambda v: v, _expr_from_py(node.args[1], strs, dicts))
            if fn.attr == "get" and len(node.args) == 1:
                # The bare 1-arg get IS the Maybe now the None<->Maybe boundary is decided:
                # map_get(k, d), flowing to a Maybe position (a return in an `-> Optional[T]`
                # function, `is None` narrowing via a `let`). Misuse as a bare value fails the
                # type/example gate rather than shipping wrong.
                return b_app(b_var("map_get"), [_expr_from_py(node.args[0], strs, dicts),
                                                _expr_from_py(fn.value, strs, dicts)])
        if isinstance(fn, ast.Name):
            # `len` of a known string is str_length (Unicode scalars — matches Python's len); of a
            # known dict, map_size.
            if fn.id == "len" and len(node.args) == 1 and _is_stringish(node.args[0], strs):
                return b_app(b_var("str_length"), [_expr_from_py(node.args[0], strs, dicts)])
            if fn.id == "len" and len(node.args) == 1 and _is_dictish(node.args[0], dicts):
                return b_app(b_var("map_size"), [_expr_from_py(node.args[0], strs, dicts)])
            # `range(n)` / `range(a, b)` -> the canonical `range` commons record by fn_ref —
            # counted iteration as DATA built by structural recursion (no new builtin), so
            # `for i in range(…)` rides the existing loop shapes and a counting `while`
            # desugars to it. A stepped `range(a, b, s)` is out of subset (honest refusal).
            if fn.id == "range" and node.args:
                if len(node.args) > 2:
                    raise BodyError("`range` with a step is out of subset")
                lo = _expr_from_py(node.args[0], strs, dicts) if len(node.args) == 2 \
                    else b_lit(_value(0))
                hi = _expr_from_py(node.args[-1], strs, dicts)
                return _fn_ref_app(RANGE_HASH, [lo, hi])
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
    if isinstance(node, ast.Tuple):
        # A tuple `(a, b, …)` -> the `tuple` construction node (>=2 elements; a 1-tuple is the
        # element, the empty tuple is unit — handled by the value layer). The heterogeneous product.
        if any(isinstance(e, ast.Starred) for e in node.elts):
            raise BodyError("starred tuple elements are out of subset")
        if len(node.elts) < 2:
            raise BodyError("a tuple needs at least two elements")
        return {"kind": "tuple", "elems": [_expr_from_py(e, strs, dicts) for e in node.elts]}
    raise BodyError(f"unsupported expression {type(node).__name__}")


def _block_from_py(stmts, strs=frozenset(), dicts=frozenset(), maybes=frozenset(), ret_maybe=False,
                   ints=frozenset(), lists=frozenset()):
    """Translate a statement sequence that must produce a value into an expression: `return r` is the
    result; `x = e; …` becomes `let x = e in …`; `if c: …`/`else`/early-return becomes `case`.
    `strs`/`dicts`/`maybes` carry the names known to hold STRINGS / DICTS / MAYBEs
    (annotation-rooted, threaded through `let`s with shadowing); `ints`/`lists` the names PROVEN
    int/list, consumed only by the truthiness desugaring in test positions (`_test_from_py`) and
    dropped conservatively on any rebinding — a name whose type is no longer proven refuses a
    truthy test rather than desugaring with the wrong falsy set. `ret_maybe` marks an
    `-> Optional[T]` function: every returned value is wrapped at the None<->Maybe boundary
    (`return None` -> the None variant; an already-Maybe expression passes through; anything else
    -> Just(...)) — Python never wraps its optionals, so the wrapping is the annotation's."""
    if not stmts:
        raise BodyError("block falls off the end without returning a value")
    head, tail = stmts[0], stmts[1:]
    if isinstance(head, ast.Return):
        if head.value is None:
            raise BodyError("bare `return` (no value)")
        # statements after a return are dead
        if ret_maybe:
            if isinstance(head.value, ast.Constant) and head.value.value is None:
                return b_variant("None")
            # `return d[k]` / `return xs[i]`: the subscript IS the Maybe — pass it through
            # unwrapped, exactly like the bare 1-arg `d.get(k)`.
            sub = _subscript_read(head.value, strs, dicts, lists)
            if sub is not None:
                return sub
            expr = _expr_from_py(head.value, strs, dicts)
            return expr if _is_maybeish(head.value, maybes, dicts) else b_variant("Just", expr)
        return _expr_from_py(head.value, strs, dicts)
    if isinstance(head, ast.Raise):
        # Raise-totalization (the None<->Maybe boundary, producing side): in a Maybe-returning
        # translation a `raise` IS the None outcome — the guard shape `if c: raise ValueError(…)`
        # becomes the None arm of the boolean case, and the record's declared result is `Maybe T`
        # (the adapter wraps it), so partiality turns into the total Maybe the language prefers
        # over `error`. Outside a Maybe-returning translation, `raise` stays out of subset.
        if ret_maybe:
            return b_variant("None")  # statements after a raise are dead
        raise BodyError("unsupported statement Raise")
    if isinstance(head, ast.Assign) and len(head.targets) == 1 \
            and isinstance(head.targets[0], (ast.Tuple, ast.List)):
        # Tuple-unpacking assignment `x, y = expr; …rest` -> `case expr of { (x, y) => rest }` —
        # the reader side of a tuple result. Targets must be plain names (no nested/starred).
        names = _tuple_target_names(head.targets[0])
        pat = {"kind": "tuple", "elems": [{"kind": "bind", "name": n} for n in names]}
        # The unpacked names are freshly bound; drop them from the type-inference sets.
        inner, inner_d, inner_m = strs - set(names), dicts - set(names), maybes - set(names)
        return {"kind": "case", "scrutinee": _expr_from_py(head.value, strs, dicts),
                "arms": [{"pattern": pat, "body": _block_from_py(tail, inner, inner_d, inner_m, ret_maybe,
                                                                 ints - set(names), lists - set(names))}]}
    if isinstance(head, ast.Assign) and len(head.targets) == 1 \
            and isinstance(head.targets[0], ast.Subscript):
        # Subscript STORE `d[k] = v` over a known dict: the total map_put rebind — no partiality,
        # no totalization (a store cannot miss). `d` stays a known dict.
        target = head.targets[0]
        if not (isinstance(target.value, ast.Name) and _is_dictish(target.value, dicts)):
            raise BodyError("subscript assignment is only in subset over a known dict")
        d = target.value.id
        update = b_app(b_var("map_put"),
                       [_expr_from_py(target.slice, strs, dicts),
                        _expr_from_py(head.value, strs, dicts), b_var(d)])
        return b_let(d, update, _block_from_py(tail, strs - {d}, dicts, maybes - {d}, ret_maybe,
                                               ints - {d}, lists - {d}))
    if isinstance(head, ast.Assign):
        if len(head.targets) != 1 or not isinstance(head.targets[0], ast.Name):
            raise BodyError("only single-name assignment targets are in subset")
        name = head.targets[0].id
        # `v = d[k]` / `v = xs[i]`: a read subscript is a MAYBE — bind the Just payload to `v` and
        # short-circuit the miss to the function's None outcome (only a Maybe-totalized function
        # may read a subscript; the adapter's trigger mirrors raise-totalization).
        sub = _subscript_read(head.value, strs, dicts, lists)
        if sub is not None:
            if not ret_maybe:
                raise BodyError("a read subscript is partial — only a Maybe-totalized function "
                                "(or one returning it directly) may bind it")
            rest = _block_from_py(tail, strs - {name}, dicts - {name}, maybes - {name}, ret_maybe,
                                  ints - {name}, lists - {name})
            return {"kind": "case", "scrutinee": sub,
                    "arms": [
                        {"pattern": {"kind": "variant", "tag": "Just",
                                     "payload": {"kind": "bind", "name": name}},
                         "body": rest},
                        {"pattern": {"kind": "variant", "tag": "None"}, "body": b_variant("None")},
                    ]}
        inner = (strs | {name}) if _is_stringish(head.value, strs) else (strs - {name})
        inner_d = (dicts | {name}) if _is_dictish(head.value, dicts) else (dicts - {name})
        inner_m = (maybes | {name}) if _is_maybeish(head.value, maybes, dicts) else (maybes - {name})
        return b_let(name, _expr_from_py(head.value, strs, dicts),
                     _block_from_py(tail, inner, inner_d, inner_m, ret_maybe,
                                    ints - {name}, lists - {name}))
    if isinstance(head, ast.AnnAssign):
        if not isinstance(head.target, ast.Name) or head.value is None:
            raise BodyError("annotated assignment must be `name: T = value`")
        name = head.target.id
        annotated_str = isinstance(head.annotation, ast.Name) and head.annotation.id == "str"
        annotated_dict = isinstance(head.annotation, ast.Name) and head.annotation.id in _DICT_ANNOTATIONS
        inner = (strs | {name}) if (annotated_str or _is_stringish(head.value, strs)) else (strs - {name})
        inner_d = (dicts | {name}) if (annotated_dict or _is_dictish(head.value, dicts)) else (dicts - {name})
        inner_m = (maybes | {name}) if (_is_optional_ann(head.annotation)
                                        or _is_maybeish(head.value, maybes, dicts)) else (maybes - {name})
        annotated_int = isinstance(head.annotation, ast.Name) and head.annotation.id == "int"
        inner_i = (ints | {name}) if annotated_int else (ints - {name})
        inner_l = (lists | {name}) if _is_list_ann(head.annotation) else (lists - {name})
        return b_let(name, _expr_from_py(head.value, strs, dicts),
                     _block_from_py(tail, inner, inner_d, inner_m, ret_maybe, inner_i, inner_l))
    if isinstance(head, ast.AugAssign):
        # `acc += e` (or -=, *=, /=, %=) re-binds `acc` to `acc <op> e` — a `let` over the rest. `acc`
        # must already be bound (a parameter or a preceding assignment). `s += t` over a known string
        # is str_concat, like binary `+`.
        if not isinstance(head.target, ast.Name):
            raise BodyError("augmented assignment must target a single name")
        name = head.target.id
        if isinstance(head.op, ast.Add) and (name in strs or _is_stringish(head.value, strs)):
            update = b_app(b_var("str_concat"), [b_var(name), _expr_from_py(head.value, strs, dicts)])
            return b_let(name, update, _block_from_py(tail, strs | {name}, dicts - {name},
                                                      maybes - {name}, ret_maybe,
                                                      ints - {name}, lists - {name}))
        if type(head.op) not in _PY_BIN:
            raise BodyError("unsupported augmented-assignment operator")
        update = _op_app(_PY_BIN[type(head.op)], [b_var(name), _expr_from_py(head.value, strs, dicts)])
        return b_let(name, update, _block_from_py(tail, strs - {name}, dicts - {name},
                                                  maybes - {name}, ret_maybe,
                                                  ints - {name}, lists - {name}))
    if isinstance(head, ast.If):
        # Narrowing: `if x is None:` / `if x is not None:` over a known Maybe becomes a `case` on
        # it — the non-None branch REBINDS x to the Just payload (Python's type narrowing made
        # explicit), and the None branch reading x (it IS None there; the translation has no
        # binding for it) is refused rather than silently wrong.
        nt = _none_test(head.test, maybes)
        if nt is not None:
            name, narrows_on_none = nt
            none_stmts = head.body if narrows_on_none else (head.orelse if head.orelse else tail)
            just_stmts = (head.orelse if head.orelse else tail) if narrows_on_none else head.body
            for s in none_stmts:
                if any(isinstance(n, ast.Name) and n.id == name and isinstance(n.ctx, ast.Load)
                       for n in ast.walk(s)):
                    raise BodyError("narrowed-to-None name read in the None branch")
            narrowed = maybes - {name}
            return {
                "kind": "case",
                "scrutinee": b_var(name),
                "arms": [
                    {"pattern": {"kind": "variant", "tag": "Just",
                                 "payload": {"kind": "bind", "name": name}},
                     "body": _block_from_py(just_stmts, strs, dicts, narrowed, ret_maybe, ints, lists)},
                    {"pattern": {"kind": "variant", "tag": "None"},
                     "body": _block_from_py(none_stmts, strs, dicts, narrowed, ret_maybe, ints, lists)},
                ],
            }
        # A boolean test translates as before; a truthy non-bool test desugars via _test_from_py
        # when an annotation proves its type, and refuses otherwise.
        test_expr = _test_from_py(head.test, strs, dicts, maybes, ints, lists)
        then_expr = _block_from_py(head.body, strs, dicts, maybes, ret_maybe, ints, lists)
        # An `else`/`elif` block is the false branch; without one, the rest of the function is.
        else_expr = _block_from_py(head.orelse if head.orelse else tail, strs, dicts, maybes,
                                   ret_maybe, ints, lists)
        return b_if(test_expr, then_expr, else_expr)
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
        if head.orelse:
            raise BodyError("`for … else` is out of subset")
        # The loop element is a single NAME, or a TUPLE `(a, b)` destructured per iteration. For a
        # tuple target we bind a fresh element name and unpack it — via `_welt` — inside every
        # lambda body that reads the element, so `for (k, v) in items: total += v` works uniformly
        # across the accumulator/append/nested/search shapes.
        if isinstance(head.target, ast.Name):
            x, elt_names = head.target.id, None
        elif isinstance(head.target, (ast.Tuple, ast.List)):
            elt_names = _tuple_target_names(head.target)
            live = {n.id for st in (head.body + list(tail)) for n in ast.walk(st) if isinstance(n, ast.Name)}
            x = "_elt"
            while x in live or x in elt_names:
                x += "_"
        else:
            raise BodyError("only `for <name>` / `for (a, b)` loops are in subset")
        src = _expr_from_py(head.iter, strs, dicts)
        return _loop_from_py(x, elt_names, src, head.body, tail, stmts, strs, dicts, maybes,
                             ret_maybe, ints, lists)
    if isinstance(head, ast.While):
        # The COUNTING while (spec/expressiveness.md, statement-subset frontier): a unit-step
        # counter against a loop-invariant bound IS iteration over an integer interval, so it
        # desugars to the `for` machinery over the canonical `range` record — every existing loop
        # shape (accumulators, guards, appends, search) then applies unchanged. Anything without a
        # recognized progress shape is refused (termination has no witness), never approximated.
        if head.orelse:
            raise BodyError("`while … else` is out of subset")
        i, src, inner_body = _counting_while(head, strs, dicts)
        return _loop_from_py(i, None, src, inner_body, tail, stmts, strs, dicts, maybes,
                             ret_maybe, ints, lists)
    raise BodyError(f"unsupported statement {type(head).__name__}")


def _counting_while(head, strs, dicts):
    """Recognize the counting `while` and return `(counter, src_expr, body_without_step)`.
    The shape: `while i <OP> bound: …; i += 1` (or `i -= 1` under a descending `>`/`>=` guard),
    where the step is the LAST statement, the counter is assigned nowhere else in the body, and
    the bound is loop-invariant (reads neither the counter nor any name the body assigns). The
    iterated values are exactly Python's:
        i <  b  ->  range(i, b)             i >  b  ->  reverse(range(b + 1, i + 1))
        i <= b  ->  range(i, b + 1)         i >= b  ->  reverse(range(b, i + 1))
    Everything else — compound guards, non-unit steps, counter mutation mid-body — refuses."""
    test = head.test
    if not (isinstance(test, ast.Compare) and len(test.ops) == 1
            and isinstance(test.left, ast.Name) and type(test.ops[0]) in (ast.Lt, ast.LtE, ast.Gt, ast.GtE)):
        raise BodyError("only counting `while i < n` loops are in subset")
    i, op, bound = test.left.id, test.ops[0], test.comparators[0]
    up = isinstance(op, (ast.Lt, ast.LtE))
    body = head.body
    step = body[-1] if body else None
    if not (isinstance(step, ast.AugAssign) and isinstance(step.target, ast.Name)
            and step.target.id == i and isinstance(step.op, ast.Add if up else ast.Sub)
            and isinstance(step.value, ast.Constant) and step.value.value == 1):
        raise BodyError("a counting `while` must end with a unit step of its counter "
                        "(`i += 1`, or `i -= 1` under a descending guard)")
    inner = body[:-1]
    assigned = set()
    for s in body:
        for n in ast.walk(s):
            if isinstance(n, (ast.Assign, ast.AugAssign, ast.AnnAssign)):
                targets = n.targets if isinstance(n, ast.Assign) else [n.target]
                assigned |= {t.id for t in targets if isinstance(t, ast.Name)}
    for s in inner:
        for n in ast.walk(s):
            if isinstance(n, (ast.Assign, ast.AugAssign, ast.AnnAssign)):
                targets = n.targets if isinstance(n, ast.Assign) else [n.target]
                if any(isinstance(t, ast.Name) and t.id == i for t in targets):
                    raise BodyError("the `while` counter is reassigned inside the loop body")
    if any(isinstance(n, ast.Name) and (n.id == i or n.id in assigned) for n in ast.walk(bound)):
        raise BodyError("the `while` bound must be loop-invariant")
    b = _expr_from_py(bound, strs, dicts)
    one = b_lit(_value(1))
    if isinstance(op, ast.Lt):
        src = _fn_ref_app(RANGE_HASH, [b_var(i), b])
    elif isinstance(op, ast.LtE):
        src = _fn_ref_app(RANGE_HASH, [b_var(i), _op_app("add", [b, one])])
    elif isinstance(op, ast.Gt):
        src = b_app(b_var("reverse"),
                    [_fn_ref_app(RANGE_HASH, [_op_app("add", [b, one]), _op_app("add", [b_var(i), one])])])
    else:
        src = b_app(b_var("reverse"),
                    [_fn_ref_app(RANGE_HASH, [b, _op_app("add", [b_var(i), one])])])
    return i, src, inner


def _loop_from_py(x, elt_names, src, body, tail, stmts, strs=frozenset(), dicts=frozenset(),
                  maybes=frozenset(), ret_maybe=False, ints=frozenset(), lists=frozenset()):
    """The shared loop translation: element `x` (or tuple names `elt_names`) drawn from the list
    expression `src` (a `for`'s iterable, or a counting `while`'s range — see `_counting_while`),
    over the four supported shapes (search / append / nested append / accumulator folds)."""
    if True:  # (kept at the original indentation for a reviewable diff)
        bound_elt = {x} if elt_names is None else set(elt_names)
        # After the loop Python leaves the element name(s) bound to the last item; none of the
        # translations do, so a tail that reads any of them is out of subset (not silently wrong).
        for s in tail:
            if any(isinstance(n, ast.Name) and n.id in bound_elt and isinstance(n.ctx, ast.Load)
                   for n in ast.walk(s)):
                raise BodyError("loop variable read after a loop")
        loop_strs, loop_dicts, loop_maybes = strs - bound_elt, dicts - bound_elt, maybes - bound_elt
        loop_ints, loop_lists = ints - bound_elt, lists - bound_elt

        def _welt(expr):
            """Wrap a lambda body that reads the element: for a tuple target, destructure the fresh
            element var into its component names; for a name target, the body is used as-is."""
            if elt_names is None:
                return expr
            return {"kind": "case", "scrutinee": b_var(x),
                    "arms": [{"pattern": {"kind": "tuple",
                                          "elems": [{"kind": "bind", "name": n} for n in elt_names]},
                              "body": expr}]}

        # Peel one optional guard `if cond: <body>` (no else) off the loop body. The guard is
        # translated ONCE here (boolean as before; annotation-proven truthiness desugared) and the
        # resulting expression reused by every loop shape below.
        orig_body = body
        guard = guard_expr = None
        if len(body) == 1 and isinstance(body[0], ast.If) and not body[0].orelse:
            guard = body[0].test
            guard_expr = _test_from_py(guard, loop_strs, loop_dicts, loop_maybes,
                                       loop_ints, loop_lists)
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
                hits_src = b_app(b_var("filter"), [b_lambda([x], _welt(guard_expr)), hits_src])
            used = bound_elt | {x} | {n.id for s in stmts for n in ast.walk(s) if isinstance(n, ast.Name)}
            hits = "hits"
            while hits in used:
                hits += "_"
            hit = b_app(b_var("head"), [b_var(hits)])
            if ret_maybe and isinstance(stmt.value, ast.Constant) and stmt.value.value is None:
                found_branch = b_variant("None")
            else:
                found = _expr_from_py(stmt.value, loop_strs, loop_dicts)
                # Bind the found element: for a tuple, destructure `head(hits)` into the component
                # names; for a name, `let x = head(hits)` (skipped when the value IS the element).
                if elt_names is not None:
                    found_branch = {"kind": "case", "scrutinee": hit,
                                    "arms": [{"pattern": {"kind": "tuple",
                                                          "elems": [{"kind": "bind", "name": n} for n in elt_names]},
                                              "body": found}]}
                else:
                    found_branch = hit if found == b_var(x) else b_let(x, hit, found)
                if ret_maybe and not _is_maybeish(stmt.value, loop_maybes, loop_dicts):
                    found_branch = b_variant("Just", found_branch)
            return b_let(hits, hits_src,
                         b_if(b_app(b_var("null"), [b_var(hits)]),
                              _block_from_py(tail, strs, dicts, maybes, ret_maybe, ints, lists),
                              found_branch))

        # Shape: list-building via `acc.append(e)`.
        if isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Call) \
                and isinstance(stmt.value.func, ast.Attribute) and stmt.value.func.attr == "append" \
                and isinstance(stmt.value.func.value, ast.Name) and len(stmt.value.args) == 1 \
                and not stmt.value.keywords:
            acc = stmt.value.func.value.id
            src2 = src
            if guard is not None:
                src2 = b_app(b_var("filter"), [b_lambda([x], _welt(guard_expr)), src2])
            elt = _expr_from_py(stmt.value.args[0], loop_strs, loop_dicts)
            mapped = src2 if (elt_names is None and elt == b_var(x)) \
                else b_app(b_var("map"), [b_lambda([x], _welt(elt)), src2])
            # append onto the accumulator's prior value; `append(nil, L) = L`, so a `[]`-seeded
            # build is just the mapped/filtered list, and a non-empty seed is honored.
            rebind = b_app(b_var("append"), [b_var(acc), mapped])
            return b_let(acc, rebind, _block_from_py(tail, strs - {acc}, dicts - {acc},
                                                     maybes - {acc}, ret_maybe,
                                                     ints - {acc}, lists - {acc}))

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
            iguard = iguard_expr = None
            if len(ibody) == 1 and isinstance(ibody[0], ast.If) and not ibody[0].orelse:
                iguard = ibody[0].test
                iguard_expr = _test_from_py(iguard, in_strs, in_dicts, loop_maybes - {i},
                                            loop_ints - {i}, loop_lists - {i})
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
            for n in (nn for s in orig_body for nn in ast.walk(s)):
                if isinstance(n, ast.Name) and n.id == acc and isinstance(n.ctx, ast.Load) \
                        and n is not receiver:
                    raise BodyError("nested loop reads its accumulator mid-loop")
            batch = inner_src
            if iguard is not None:
                batch = b_app(b_var("filter"), [b_lambda([i], iguard_expr), batch])
            elt = _expr_from_py(ibody[0].value.args[0], in_strs, in_dicts)
            if elt != b_var(i):
                batch = b_app(b_var("map"), [b_lambda([i], elt), batch])
            outer_src = src
            if guard is not None:
                outer_src = b_app(b_var("filter"), [b_lambda([x], _welt(guard_expr)), outer_src])
            step = b_lambda([acc, x], _welt(b_app(b_var("append"), [b_var(acc), batch])))
            fold = b_app(b_var("foldl"), [step, b_var(acc), outer_src])
            return b_let(acc, fold, _block_from_py(tail, strs - {acc}, dicts - {acc},
                                                   maybes - {acc}, ret_maybe,
                                                   ints - {acc}, lists - {acc}))

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
            if len(accs) > 1 and len(set(accs)) != len(accs):
                raise BodyError("duplicate accumulator in a multi-accumulator loop")
            accset = set(accs)
            dependent = False
            if len(accs) > 1:
                for stmt in body:
                    target = stmt.target if isinstance(stmt, ast.AugAssign) else stmt.targets[0]
                    others = accset - {target.id}
                    if any(isinstance(n, ast.Name) and n.id in others for n in ast.walk(stmt.value)):
                        dependent = True
                if guard is not None \
                        and any(isinstance(n, ast.Name) and n.id in accset for n in ast.walk(guard)):
                    dependent = True

            if dependent:
                # DEPENDENT accumulators — an update (or the guard) reads another accumulator's
                # mid-loop value, which N separate folds can't reproduce. Thread ALL accumulators
                # through ONE fold with a TUPLE accumulator, updating them in source order within a
                # step (so a later update sees an earlier one's new value, as Python does):
                #   let (s, c) = foldl(\_acc x -> case _acc of (s, c) =>
                #                        let _g = <guard at iter start> in
                #                        let s = case _g of {true => s'; false => s} in
                #                        let c = case _g of {true => c'; false => c} in (s, c),
                #                      (s0, c0), src)
                #   in <rest>
                used = accset | bound_elt | {x} | {n.id for s in stmts for n in ast.walk(s) if isinstance(n, ast.Name)}

                def _fresh(base):
                    while base in used:
                        base += "_"
                    used.add(base)
                    return base
                accp, gvar, foldvar = _fresh("_acc"), _fresh("_g"), _fresh("_folded")
                acc_pat = {"kind": "tuple", "elems": [{"kind": "bind", "name": a} for a in accs]}
                # Evaluate the guard once at iteration start (over the destructured accs), if present;
                # then apply the updates in source order (each later one sees earlier rebindings).
                step = {"kind": "tuple", "elems": [b_var(a) for a in accs]}
                for acc, update in reversed(list(zip(accs, updates))):
                    if guard is not None:
                        update = b_if(b_var(gvar), update, b_var(acc))
                    step = b_let(acc, update, step)
                if guard is not None:
                    step = b_let(gvar, guard_expr, step)
                destructure = {"kind": "case", "scrutinee": b_var(accp),
                               "arms": [{"pattern": acc_pat, "body": step}]}
                seed = {"kind": "tuple", "elems": [b_var(a) for a in accs]}
                fold = b_app(b_var("foldl"), [b_lambda([accp, x], _welt(destructure)), seed, src])
                result = _block_from_py(tail, strs, dicts, maybes - accset, ret_maybe,
                                        ints - accset, lists - accset)
                unpack = {"kind": "case", "scrutinee": b_var(foldvar),
                          "arms": [{"pattern": acc_pat, "body": result}]}
                return b_let(foldvar, fold, unpack)

            result = _block_from_py(tail, strs, dicts, maybes - accset, ret_maybe,
                                    ints - accset, lists - accset)
            for acc, update in reversed(list(zip(accs, updates))):
                # A guarded step keeps the accumulator unchanged on the false branch.
                if guard is not None:
                    update = b_if(guard_expr, update, b_var(acc))
                fold = b_app(b_var("foldl"), [b_lambda([acc, x], _welt(update)), b_var(acc), src])
                result = b_let(acc, fold, result)
            return result
        raise BodyError("loop body must be an accumulator assignment or an `.append(…)`")


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


def _is_optional_ann(ann):
    """Whether an annotation AST is an optional type — `Optional[T]`, `T | None`, `None | T`, or
    `Union[..., None, ...]` — the roots of the known-Maybe inference."""
    if isinstance(ann, ast.Subscript):
        base = ann.value.attr if isinstance(ann.value, ast.Attribute) else getattr(ann.value, "id", None)
        if base == "Optional":
            return True
        if base == "Union":
            members = ann.slice.elts if isinstance(ann.slice, ast.Tuple) else [ann.slice]
            return any(isinstance(m, ast.Constant) and m.value is None for m in members)
        return False
    if isinstance(ann, ast.BinOp) and isinstance(ann.op, ast.BitOr):
        return any(isinstance(side, ast.Constant) and side.value is None
                   for side in (ann.left, ann.right)) \
            or _is_optional_ann(ann.left) or _is_optional_ann(ann.right)
    return False


def _optional_annotated_params(func):
    """The parameter names annotated optional — the roots of the known-Maybe inference behind the
    None<->Maybe boundary translations (`is None` narrowing, Just-wrapped returns)."""
    a = func.args
    return frozenset(p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))
                     if _is_optional_ann(p.annotation))


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


_LIST_ANNOTATIONS = ("list", "List", "Sequence")


def _is_list_ann(ann):
    if isinstance(ann, ast.Name):
        return ann.id in _LIST_ANNOTATIONS
    return isinstance(ann, ast.Subscript) and isinstance(ann.value, ast.Name) \
        and ann.value.id in _LIST_ANNOTATIONS


def _int_annotated_params(func):
    """The parameter names annotated `int` — with `_list_annotated_params`, the roots of the
    truthiness desugaring (the falsy set is type-dependent, so only a proven type may desugar)."""
    a = func.args
    return frozenset(p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))
                     if isinstance(p.annotation, ast.Name) and p.annotation.id == "int")


def _list_annotated_params(func):
    """The parameter names annotated as lists (`list`, `list[...]`, `List[...]`, `Sequence[...]`)."""
    a = func.args
    return frozenset(p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))
                     if _is_list_ann(p.annotation))


def function_raises(func):
    """Whether the function's own body contains a `raise` — the trigger for raise-totalization
    (the lifted body returns `Maybe T` with raise-branches as `None`, and the adapter wraps the
    declared result type to match)."""
    return any(isinstance(n, ast.Raise) for n in ast.walk(func))


def function_subscript_reads(func):
    """Whether the function's body READS a subscript (`d[k]` / `xs[i]` in Load context) — with
    `function_raises`, a trigger for Maybe-totalization: a raising access IS the None outcome.
    Annotation positions (`dict[str, int]`, `list[int]`) do not count, and a subscript STORE
    (`d[k] = v`, the total map_put rebind) needs no totalization."""
    ann = []
    a = func.args
    for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs)):
        if p.annotation is not None:
            ann.append(p.annotation)
    if func.returns is not None:
        ann.append(func.returns)
    for node in ast.walk(func):
        if isinstance(node, ast.AnnAssign) and node.annotation is not None:
            ann.append(node.annotation)
    in_annotations = {id(x) for t in ann for x in ast.walk(t)}
    return any(isinstance(n, ast.Subscript) and isinstance(n.ctx, ast.Load)
               and id(n) not in in_annotations for n in ast.walk(func))


def body_ast_from_py(func):
    """An *executable* body AST for a Python function whose body is in the supported subset (a
    `lambda` over its parameters; a bare expression for a 0-parameter function), or None otherwise.
    A function that raises is TOTALIZED: its translation returns `Maybe T` (raise -> the None
    variant, returns Just-wrapped), and the adapter wraps the declared result type to match —
    unless it is already `-> Optional[…]`, where collapsing a raise and a None return into one
    Maybe would silently merge two distinct outcomes, so the combination stays out of subset."""
    body = func.body
    start = 1 if (body and isinstance(body[0], ast.Expr)
                  and isinstance(body[0].value, ast.Constant)
                  and isinstance(body[0].value.value, str)) else 0
    stmts = body[start:]
    if not stmts:
        return None
    # Subscript READS are partiality like `raise` — either triggers the Maybe totalization, and
    # either combined with an explicit `-> Optional[T]` stays out of subset (collapsing a missing
    # key/index or a raise with a returned None would silently merge two distinct outcomes).
    partial = function_raises(func) or function_subscript_reads(func)
    ret_opt = _is_optional_ann(func.returns)
    if partial and ret_opt:
        return None
    try:
        expr = _block_from_py(stmts, _str_annotated_params(func), _dict_annotated_params(func),
                              _optional_annotated_params(func), ret_opt or partial,
                              _int_annotated_params(func), _list_annotated_params(func))
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


# Haskell infix operator -> (Nova builtin, binding precedence). Chosen to match the builtins the
# Python/TS front ends emit (`_PY_BIN`/`_PY_CMP`/`_PY_BOOL`), so the SAME function ingested from any
# adapter hashes to the SAME record. `/=` is Haskell not-equal (-> neq). Precedence follows the Haskell
# Report defaults so `a + b * c` associates correctly.
_HS_OPS = {
    "||": ("or", 2), "&&": ("and", 3),
    "==": ("eq", 4), "/=": ("neq", 4), "<=": ("le", 4), ">=": ("ge", 4), "<": ("lt", 4), ">": ("gt", 4),
    "+": ("add", 6), "-": ("sub", 6), "*": ("mul", 7),
}


def _hs_split_binding(text):
    """Split `f x y = rhs` at the DEFINING `=` — the first top-level lone `=`, i.e. not part of a `==`,
    `<=`, `>=`, or `/=` operator in the RHS. Returns (lhs, rhs) or None."""
    depth = 0
    for i, c in enumerate(text):
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif c == "=" and depth == 0:
            prev = text[i - 1] if i > 0 else " "
            nxt = text[i + 1] if i + 1 < len(text) else " "
            if prev not in "<>=/!" and nxt != "=":
                return text[:i], text[i + 1:]
    return None


def _hs_app_chunk(text):
    """One operand of a Haskell operator expression: a bare atom, or a flat application of atoms
    (`f a b`). Parens, sections, and lambdas -> None (the honest boundary — nested structure needs a
    real parser)."""
    text = text.strip()
    if not text or "(" in text or ")" in text or "\\" in text:
        return None
    toks = text.split()
    if len(toks) == 1:
        return _atom(toks[0])
    head = _atom(toks[0])
    if head["kind"] != "var":
        return None
    return b_app(head, [_atom(t) for t in toks[1:]])


def _hs_tokenize_ops(text):
    """Split a Haskell RHS into alternating operand chunks and infix operators at paren depth 0.
    Returns (operands, ops) with len(operands) == len(ops)+1, or None on an empty operand, unbalanced
    parens, or a leading operator (a unary/section shape this subset doesn't model)."""
    operands, ops, buf, depth, i = [], [], [], 0, 0
    while i < len(text):
        c = text[i]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0:
            pair = text[i:i + 2]
            if pair in _HS_OPS:
                operands.append("".join(buf).strip()); ops.append(pair); buf = []; i += 2; continue
            if c in _HS_OPS:
                operands.append("".join(buf).strip()); ops.append(c); buf = []; i += 1; continue
        buf.append(c)
        i += 1
    if depth != 0:
        return None
    operands.append("".join(buf).strip())
    if any(o == "" for o in operands):
        return None
    return operands, ops


def _hs_build_ops(operands, ops):
    """Combine operand chunks and infix operators into one body AST by operator precedence (repeatedly
    reducing the leftmost highest-precedence operator = left-associative within a level). None if any
    operand isn't a supported atom/application."""
    atoms = []
    for chunk in operands:
        a = _hs_app_chunk(chunk)
        if a is None:
            return None
        atoms.append(a)
    ops = list(ops)
    while ops:
        top = max(_HS_OPS[o][1] for o in ops)
        k = next(idx for idx, o in enumerate(ops) if _HS_OPS[o][1] == top)
        atoms[k:k + 2] = [_op_app(_HS_OPS[ops[k]][0], [atoms[k], atoms[k + 1]])]
        del ops[k]
    return atoms[0]


def body_ast_from_hs(name, equation_text):
    """An *executable* body AST (a `lambda` over the equation's parameters) for a single-clause Haskell
    equation whose RHS is a bare variable, a flat application of atoms (`f a b`), or an infix
    expression over such operands using the arithmetic/comparison/boolean operators in `_HS_OPS`
    (`x + y`, `length xs + 1`, `x == 0`, `a && b`). Guards, sections, lambdas, `let`/`case`/`if`,
    parenthesised sub-expressions, multi-line / multi-clause bodies -> None."""
    if not equation_text or "\n" in equation_text:
        return None
    text = equation_text.strip()
    if "|" in text.replace("||", ""):  # guards — but keep the boolean-or operator `||`
        return None
    split = _hs_split_binding(text)
    if split is None:
        return None
    lhs, rhs = split[0].strip(), split[1].strip()
    lhs_toks = lhs.split()
    if not lhs_toks or lhs_toks[0] != name:
        return None
    params = lhs_toks[1:]  # the equation's parameters, e.g. `f x y = …` -> [x, y]
    try:
        toks = _hs_tokenize_ops(rhs)
        if toks is None:
            return None
        expr = _hs_build_ops(*toks)
        if expr is None:
            return None
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


def _ts_normalize_expr(text):
    """Rewrite the TS-only operator spellings the Python-expression parser rejects into their
    value-domain equivalents so the shared builder can parse them: strict `===`/`!==` are ordinary
    equality/inequality over the coercion-free value domain (`==`/`!=`), and the logical `&&`/`||` are
    Python's `and`/`or`. A wrong guess still fails the example gate rather than shipping — this only
    widens what parses, never what's accepted."""
    text = text.replace("===", "==").replace("!==", "!=")
    return text.replace("&&", " and ").replace("||", " or ")


def _ts_expr(rhs, strs):
    """Build a body expression from a TS *expression* (an arrow's expression body, or a `return`'s
    operand), reusing the shared Python-expression builder over the common subset. None if it isn't in
    subset or doesn't parse (TS-only `?:` / `!` etc.)."""
    if not rhs:
        return None
    try:
        return _expr_from_py(ast.parse(_ts_normalize_expr(rhs), mode="eval").body, strs)
    except (SyntaxError, ValueError, BodyError):
        return None


def _ts_block_expr(block_text, strs):
    """The body of a TS arrow BLOCK that is `[const|let NAME [: T] = EXPR; …] return EXPR;` — the common
    single-`return` shape. Leading `const`/`let` bindings become nested `let` nodes; the trailing
    `return` supplies the body. None for any other statement shape (loops, branches, reassignment,
    multiple/early returns) — the honest subset boundary."""
    inner = block_text.strip()
    if inner.startswith("{"):
        inner = inner[1:]
    if inner.endswith("}"):
        inner = inner[:-1]
    stmts = [s.strip() for s in split_top(inner, ";") if s.strip()]
    if not stmts or not stmts[-1].startswith("return"):
        return None
    ret_expr = stmts[-1][len("return"):].strip()
    bindings = []
    for st in stmts[:-1]:
        m = re.match(r"^(?:const|let)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(.+)$", st, re.DOTALL)
        if not m:
            return None
        bindings.append((m.group(1), m.group(2).strip()))
    expr = _ts_expr(ret_expr, strs)
    if expr is None:
        return None
    try:
        for nm, val in reversed(bindings):
            vexpr = _ts_expr(val, strs)
            if vexpr is None:
                return None
            expr = b_let(nm, vexpr, expr)
    except BodyError:
        return None
    return expr


def _ts_top_group(s, opener, closer, last):
    """The (open, close) indices of the FIRST (or, if `last`, the LAST) balanced top-level `opener…
    closer` group in `s`, or None. Used to find a declaration's parameter list (first `(…)`) and its
    body (LAST `{…}` — a `: { … }` object return type sorts before the body block)."""
    depth, start, found = 0, None, None
    for i, c in enumerate(s):
        if c == opener:
            if depth == 0:
                start = i
            depth += 1
        elif c == closer and depth > 0:
            depth -= 1
            if depth == 0:
                found = (start, i)
                if not last:
                    return found
    return found


def body_ast_from_ts(name, slice_text):
    """An *executable* body AST (a `lambda` over the parameters) for a TypeScript function — an arrow
    expression body `(x) => expr`, an arrow or `function`-declaration single-`return` block body
    `{ … return expr; }` (with optional leading `const`/`let` bindings), covering `export function f`,
    `function` expressions, and arrow forms alike. TypeScript expression syntax coincides with Python's
    for the supported subset (identifiers, literals, arithmetic/comparison/boolean operators, calls,
    member access, strict-equality), so expressions are parsed with Python's `ast` and reused via
    `_expr_from_py`. Block bodies beyond the single-`return` shape and TS-only expression syntax
    (`?:`, `!`) that doesn't parse as a Python expression -> None."""
    if not slice_text:
        return None
    idx = slice_text.find("=>")
    if idx >= 0:
        # Arrow: parameters before `=>`, an expression or block body after.
        params = _ts_params(slice_text[:idx])
        if params is None:
            return None
        strs = _ts_string_params(slice_text[:idx])
        rhs = slice_text[idx + 2:].strip().rstrip(";").strip()
        expr = _ts_block_expr(rhs, strs) if rhs.startswith("{") else _ts_expr(rhs, strs)
    else:
        # `function`-declaration / function-expression: params are the first top-level `(…)`, the body
        # is the LAST top-level `{…}` (any `: { … }` object return type sorts before it).
        pg = _ts_top_group(slice_text, "(", ")", last=False)
        bg = _ts_top_group(slice_text, "{", "}", last=True)
        if pg is None or bg is None or bg[0] < pg[1]:
            return None
        prefix = slice_text[:pg[1] + 1]
        params = _ts_params(prefix)
        if params is None:
            return None
        strs = _ts_string_params(prefix)
        expr = _ts_block_expr(slice_text[bg[0]:bg[1] + 1], strs)
    if expr is None:
        return None
    return b_lambda(params, expr) if params else expr
