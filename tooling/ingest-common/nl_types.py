"""Nova Lingua type-expression AST builder (spec/type-expression.schema.json).

The reusable engine for "higher-fidelity" ingestion: build STRUCTURED type ASTs (for `signature.type`
in v0.2 function records) instead of source-flavored strings. Pure stdlib.

Two layers:
  - language-neutral: AST node constructors (var/builtin/fn/apply/tuple/forall/ref/record/sum), a
    fresh/named type-variable allocator, and `quantify` which wraps a body in a single rank-1 forall
    over the variables it uses (sorted, the canonical form). Each adapter supplies its own front-end.
  - a **Python** front-end: `python_function_type(func_ast)` maps a `def`'s annotations to a type AST.

Honesty rule: the builtin vocabulary is a closed set with NO "unknown". Anything we can't faithfully
represent — an unannotated parameter, `Any`, a general `Union[A, B]`, a user-defined class, `*args` —
becomes a FRESH `forall`-bound type variable (distinct per unknown position, stable per named source
type). That is well-formed and honestly "parametric / unconstrained"; we never invent a concrete type.
"""

import ast

ATOMIC = {"bool", "int", "nat", "float", "string", "bytes", "unit", "never"}
CTOR = {"List", "Maybe", "Result", "Map", "Set"}

# Python type name -> Nova Lingua atomic builtin.
_PY_ATOMIC = {"int": "int", "bool": "bool", "float": "float", "str": "string",
              "bytes": "bytes", "bytearray": "bytes", "None": "unit", "NoneType": "unit"}


# --- node constructors -------------------------------------------------------------------------

def var(name):
    return {"kind": "var", "name": name}


def builtin(name):
    return {"kind": "builtin", "name": name}


def ref(target):
    return {"kind": "ref", "target": target}


def fn(params, result):
    return {"kind": "fn", "params": params, "result": result}


def apply(ctor, args):
    return {"kind": "apply", "ctor": ctor, "args": args}


def tuple_(elems):
    return {"kind": "tuple", "elems": elems}


def quantify(body, used_vars):
    """Wrap `body` in a rank-1 forall over `used_vars` (sorted + unique, the canonical form). No vars
    -> the body unquantified."""
    names = sorted(set(used_vars))
    return {"kind": "forall", "vars": names, "body": body} if names else body


# --- type-variable allocation ------------------------------------------------------------------

class VarCtx:
    """Allocates type-variable names: `named_var` is stable per source name (so a TypeVar or class
    used twice maps to one variable, preserving the relationship); `fresh_var` is a brand-new variable
    for each genuinely-unknown position. `used` collects every variable for the enclosing forall."""

    def __init__(self):
        self.named = {}
        self.used = []
        self._n = 0

    def _alloc(self):
        i, self._n = self._n, self._n + 1
        letter = chr(ord("a") + i % 26)
        return letter if i < 26 else f"{letter}{i // 26}"

    def _note(self, v):
        if v not in self.used:
            self.used.append(v)
        return v

    def fresh_var(self):
        return self._note(self._alloc())

    def named_var(self, source_name):
        if source_name not in self.named:
            self.named[source_name] = self._alloc()
        return self._note(self.named[source_name])


# --- Python front-end --------------------------------------------------------------------------

def _is_ellipsis(node):
    return isinstance(node, ast.Constant) and node.value is Ellipsis


def _slice_args(slice_node):
    return list(slice_node.elts) if isinstance(slice_node, ast.Tuple) else [slice_node]


def _name_to_ast(name, ctx):
    if name in _PY_ATOMIC:
        return builtin(_PY_ATOMIC[name])
    if name in ("Any", "object"):
        return var(ctx.fresh_var())
    if name in ("list", "List", "Sequence", "Iterable"):
        return apply(builtin("List"), [var(ctx.fresh_var())])
    if name in ("set", "Set", "frozenset", "FrozenSet"):
        return apply(builtin("Set"), [var(ctx.fresh_var())])
    if name in ("dict", "Dict", "Mapping", "MutableMapping"):
        return apply(builtin("Map"), [var(ctx.fresh_var()), var(ctx.fresh_var())])
    if name in ("tuple", "Tuple"):
        return var(ctx.fresh_var())                      # an un-parameterised tuple is unknown shape
    return var(ctx.named_var(name))                      # TypeVar or user class -> a stable variable


def _union_to_ast(members, ctx):
    has_none = any(isinstance(m, ast.Constant) and m.value is None for m in members)
    rest = [m for m in members if not (isinstance(m, ast.Constant) and m.value is None)]
    if not rest:
        return builtin("unit")
    inner = annotation_to_ast(rest[0], ctx) if len(rest) == 1 else var(ctx.fresh_var())
    return apply(builtin("Maybe"), [inner]) if has_none else inner


def _subscript_to_ast(node, ctx):
    base = node.value.attr if isinstance(node.value, ast.Attribute) else getattr(node.value, "id", None)
    args = _slice_args(node.slice)
    if base == "Optional":
        return apply(builtin("Maybe"), [annotation_to_ast(args[0], ctx)])
    if base == "Union":
        return _union_to_ast(args, ctx)
    if base in ("list", "List", "Sequence", "Iterable", "FrozenSet"):
        return apply(builtin("List" if base != "FrozenSet" else "Set"), [annotation_to_ast(args[0], ctx)])
    if base in ("set", "Set", "frozenset"):
        return apply(builtin("Set"), [annotation_to_ast(args[0], ctx)])
    if base in ("dict", "Dict", "Mapping", "MutableMapping"):
        return apply(builtin("Map"), [annotation_to_ast(args[0], ctx), annotation_to_ast(args[1], ctx)])
    if base in ("tuple", "Tuple"):
        if len(args) == 2 and _is_ellipsis(args[1]):     # tuple[T, ...] is homogeneous
            return apply(builtin("List"), [annotation_to_ast(args[0], ctx)])
        elems = [annotation_to_ast(a, ctx) for a in args if not _is_ellipsis(a)]
        if not elems:
            return builtin("unit")
        return elems[0] if len(elems) == 1 else tuple_(elems)
    if base == "Callable":
        if len(args) == 2 and isinstance(args[0], ast.List):
            return fn([annotation_to_ast(a, ctx) for a in args[0].elts], annotation_to_ast(args[1], ctx))
        return var(ctx.fresh_var())                      # Callable[..., R] / unknown shape
    return var(ctx.fresh_var())                          # unknown generic


def annotation_to_ast(node, ctx):
    """Map one Python annotation AST node to a Nova Lingua type AST. `node is None` (unannotated) ->
    a fresh variable."""
    if node is None:
        return var(ctx.fresh_var())
    if isinstance(node, ast.Constant):
        if node.value is None:
            return builtin("unit")
        if isinstance(node.value, str):
            return var(ctx.named_var(node.value))        # a string forward-reference to a named type
        return var(ctx.fresh_var())
    if isinstance(node, ast.Name):
        return _name_to_ast(node.id, ctx)
    if isinstance(node, ast.Attribute):
        return _name_to_ast(node.attr, ctx)
    if isinstance(node, ast.Subscript):
        return _subscript_to_ast(node, ctx)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return _union_to_ast([node.left, node.right], ctx)
    return var(ctx.fresh_var())


def python_function_type(func_node):
    """Build the type AST for a Python `def`/`async def` (ast.FunctionDef): a `fn` over its positional
    parameters and return, quantified over any type variables introduced. *args/**kwargs and keyword-
    only params are omitted (matching the adapters' positional arity)."""
    ctx = VarCtx()
    a = func_node.args
    params = [annotation_to_ast(arg.annotation, ctx) for arg in (a.posonlyargs + a.args)]
    result = annotation_to_ast(func_node.returns, ctx)
    return quantify(fn(params, result), ctx.used)


def module_types(path):
    """{public_func_name: type_ast} for every top-level function in a .py file."""
    tree = ast.parse(open(path, encoding="utf-8").read())
    return {n.name: python_function_type(n) for n in tree.body
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and not n.name.startswith("_")}


if __name__ == "__main__":
    import json
    import sys
    print(json.dumps(module_types(sys.argv[1]), indent=2))
