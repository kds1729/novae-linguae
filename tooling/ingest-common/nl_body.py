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
  * accumulator loops — `for x in src: acc = update` → `foldl(\acc x -> update, acc, src)`.
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

# Python operator -> Nova builtin op name (shared with the predicate vocabulary).
_PY_CMP = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq", ast.NotEq: "neq"}
_PY_BIN = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div", ast.Mod: "mod"}
_PY_BOOL = {ast.And: "and", ast.Or: "or"}
# Python builtin call -> Nova builtin (unary, unambiguous; arity-ambiguous ones like min/max excluded).
_PY_CALL = {"len": "length", "abs": "abs"}


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


def _expr_from_py(node):
    if isinstance(node, ast.IfExp):
        if not _is_boolish(node.test):
            raise BodyError("non-boolean ternary test (Python truthiness is not representable)")
        return b_if(_expr_from_py(node.test), _expr_from_py(node.body), _expr_from_py(node.orelse))
    if isinstance(node, ast.ListComp):
        # [elt for v in src (if cond)] -> map(\v -> elt, filter(\v -> cond, src))  (builtins, no loop)
        if len(node.generators) != 1:
            raise BodyError("only single-generator comprehensions are in subset")
        gen = node.generators[0]
        if getattr(gen, "is_async", 0) or not isinstance(gen.target, ast.Name) or len(gen.ifs) > 1:
            raise BodyError("comprehension shape out of subset")
        var = gen.target.id
        src = _expr_from_py(gen.iter)
        if gen.ifs:
            if not _is_boolish(gen.ifs[0]):
                raise BodyError("comprehension filter must be boolean")
            src = b_app(b_var("filter"), [b_lambda([var], _expr_from_py(gen.ifs[0])), src])
        elt = _expr_from_py(node.elt)
        if elt == b_var(var):
            return src  # `[v for v in src ...]` is the (filtered) source — no identity map
        return b_app(b_var("map"), [b_lambda([var], elt), src])
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
            fnexpr = b_var(_PY_CALL.get(fn.id, fn.id))  # map len->length, abs->abs; else as-named
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


def _block_from_py(stmts):
    """Translate a statement sequence that must produce a value into an expression: `return r` is the
    result; `x = e; …` becomes `let x = e in …`; `if c: …`/`else`/early-return becomes `case`."""
    if not stmts:
        raise BodyError("block falls off the end without returning a value")
    head, tail = stmts[0], stmts[1:]
    if isinstance(head, ast.Return):
        if head.value is None:
            raise BodyError("bare `return` (no value)")
        return _expr_from_py(head.value)  # statements after a return are dead
    if isinstance(head, ast.Assign):
        if len(head.targets) != 1 or not isinstance(head.targets[0], ast.Name):
            raise BodyError("only single-name assignment targets are in subset")
        return b_let(head.targets[0].id, _expr_from_py(head.value), _block_from_py(tail))
    if isinstance(head, ast.AnnAssign):
        if not isinstance(head.target, ast.Name) or head.value is None:
            raise BodyError("annotated assignment must be `name: T = value`")
        return b_let(head.target.id, _expr_from_py(head.value), _block_from_py(tail))
    if isinstance(head, ast.AugAssign):
        # `acc += e` (or -=, *=, /=, %=) re-binds `acc` to `acc <op> e` — a `let` over the rest. `acc`
        # must already be bound (a parameter or a preceding assignment).
        if not isinstance(head.target, ast.Name):
            raise BodyError("augmented assignment must target a single name")
        if type(head.op) not in _PY_BIN:
            raise BodyError("unsupported augmented-assignment operator")
        name = head.target.id
        update = _op_app(_PY_BIN[type(head.op)], [b_var(name), _expr_from_py(head.value)])
        return b_let(name, update, _block_from_py(tail))
    if isinstance(head, ast.If):
        if not _is_boolish(head.test):
            raise BodyError("non-boolean `if` test (Python truthiness is not representable)")
        then_expr = _block_from_py(head.body)
        # An `else`/`elif` block is the false branch; without one, the rest of the function is.
        else_expr = _block_from_py(head.orelse if head.orelse else tail)
        return b_if(_expr_from_py(head.test), then_expr, else_expr)
    if isinstance(head, ast.For):
        # An accumulator loop `for <x> in <src>: <acc> = <update>` is a left fold over `src`. `acc`
        # must already be bound (a preceding `acc = init` -> let), and the fold re-binds it:
        #   acc = foldl(\acc x -> update, acc, src) ; <rest>
        if head.orelse or not isinstance(head.target, ast.Name):
            raise BodyError("only `for <name> in <src>:` accumulator loops are in subset")
        if len(head.body) != 1 or not isinstance(head.body[0], (ast.Assign, ast.AugAssign)):
            raise BodyError("loop body must be a single accumulator assignment")
        asg = head.body[0]
        if isinstance(asg, ast.AugAssign):
            # `for x in src: acc += f(x)` -> foldl(\acc x -> acc <op> f(x), acc, src) — the common
            # sum/product/count idiom, equivalent to the explicit `acc = acc <op> f(x)` form below.
            if not isinstance(asg.target, ast.Name):
                raise BodyError("accumulator assignment must target a single name")
            if type(asg.op) not in _PY_BIN:
                raise BodyError("unsupported augmented-assignment operator")
            acc, x = asg.target.id, head.target.id
            update = _op_app(_PY_BIN[type(asg.op)], [b_var(acc), _expr_from_py(asg.value)])
        else:
            if len(asg.targets) != 1 or not isinstance(asg.targets[0], ast.Name):
                raise BodyError("accumulator assignment must target a single name")
            acc, x = asg.targets[0].id, head.target.id
            update = _expr_from_py(asg.value)
        fold = b_app(b_var("foldl"), [b_lambda([acc, x], update), b_var(acc), _expr_from_py(head.iter)])
        return b_let(acc, fold, _block_from_py(tail))
    raise BodyError(f"unsupported statement {type(head).__name__}")


def _fixed_param_names(func):
    a = func.args
    return [p.arg for p in (list(a.posonlyargs) + list(a.args) + list(a.kwonlyargs))]


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
        expr = _block_from_py(stmts)
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
        expr = _expr_from_py(node)
    except (SyntaxError, ValueError, BodyError):
        return None
    return b_lambda(params, expr) if params else expr
