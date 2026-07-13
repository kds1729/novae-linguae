"""Type-directed argument synthesis for OBSERVED worked examples.

A fully-annotated function whose source documents no doctest has a structured type but
nothing honest to put in a v0.2 record's `examples` — the annotation licenses the record,
it does not supply a value (the same license/observe split as the OpenAPI adapter's
schema-derived projections, spec/expressiveness.md). This module supplies the *arguments*
of an observation: small, fixed, deterministic Python values generated from the declared
parameter types. Executing the real function on them (the adapter's `--exec-examples`
gate) observes the result; the lifted body is then held to that observation by
`run`/`certify` — a lifting that disagrees with the source's real semantics fails rather
than publishes.

Two argument sets per function (two examples beat one; both fixed constants so records
stay byte-reproducible). A type with no concrete inhabitant here — a type variable, a
function, a Set, an un-parameterised tuple — raises `SynthError` and the caller falls
back to v0.1: we never guess at a value a type does not determine.
"""


class SynthError(Exception):
    pass


# Two fixed value palettes — variant 0 and variant 1 — per atomic type.
_ATOMIC = {
    "int": (3, -2),
    "nat": (2, 5),
    "float": (0.5, 2.25),
    "bool": (True, False),
    "string": ("hello world", "abc"),
    "bytes": (b"hello", b"\x01\x02"),
    "unit": (None, None),
}


def _apply_parts(t):
    if t.get("kind") == "apply" and (t.get("ctor") or {}).get("kind") == "builtin":
        return t["ctor"]["name"], t.get("args") or []
    return None, []


def py_value(type_ast, variant=0):
    """A deterministic Python inhabitant of a Nova Lingua type AST. Raises SynthError
    when the type does not determine one."""
    if not isinstance(type_ast, dict):
        raise SynthError("malformed type")
    kind = type_ast.get("kind")
    if kind == "builtin":
        name = type_ast.get("name")
        if name in _ATOMIC:
            return _ATOMIC[name][variant % 2]
        raise SynthError(f"no synthesized inhabitant for builtin `{name}`")
    if kind == "tuple":
        elems = type_ast.get("elems") or []
        if not elems:
            raise SynthError("empty tuple type")
        return tuple(py_value(e, variant) for e in elems)
    if kind == "apply":
        ctor, args = _apply_parts(type_ast)
        if ctor == "List" and args:
            return [py_value(args[0], variant), py_value(args[0], variant + 1)] if variant == 0 \
                else [py_value(args[0], 1)]
        if ctor == "Maybe" and args:
            # Variant 0 exercises the Just case; variant 1 the None case.
            return py_value(args[0], 0) if variant == 0 else None
        if ctor == "Map" and len(args) == 2:
            if not (args[0].get("kind") == "builtin" and args[0].get("name") == "string"):
                raise SynthError("only string-keyed maps are synthesized")
            return {"a": py_value(args[1], variant), "b": py_value(args[1], variant + 1)}
        raise SynthError(f"no synthesized inhabitant for `{ctor}`")
    raise SynthError(f"no synthesized inhabitant for kind `{kind}`")


def synth_args(param_types, sets=2):
    """Up to `sets` deterministic argument tuples for the given parameter types.
    Raises SynthError if any parameter type has no synthesized inhabitant."""
    return [[py_value(t, v) for t in param_types] for v in range(sets)]
