"""Nova Lingua value-expression AST builder (spec/value-expression.schema.json).

Maps a Python value to the structured value AST used for `examples.args[i]` / `examples.result` in
v0.2 function records. Pure stdlib; shared by the ingest adapters and the example-enrichment step.

Eleven value kinds: bool, int, nat, float, string, bytes, unit, list, tuple, record, variant, fn_ref.
Not every Python value is representable — there is **no `set` and no `Map` value kind**, and record
field names must be lowercase identifiers — so unrepresentable values raise `ValueEncodeError` and the
caller skips that example. We never fabricate or lossily coerce a value; an example is either real and
encodable or it is omitted.
"""

import base64
import math
import re

_BIGINT = 1 << 53           # |n| >= 2^53 must be a decimal string (value schema / canonical form)
_FIELD = re.compile(r"^[a-z][a-zA-Z0-9_]*$")


class ValueEncodeError(Exception):
    pass


def _int_lit(n):
    return str(n) if abs(n) >= _BIGINT else n


def _is_nat(t):
    return isinstance(t, dict) and t.get("kind") == "builtin" and t.get("name") == "nat"


def _ctor_arg(t, ctor):
    """The first type argument of an `apply` of builtin `ctor` (e.g. the element type of `List a`)."""
    if isinstance(t, dict) and t.get("kind") == "apply":
        c = t.get("ctor") or {}
        if c.get("kind") == "builtin" and c.get("name") == ctor and t.get("args"):
            return t["args"][0]
    return None


def to_value_ast(value, expected=None):
    """Encode a Python value as a Nova Lingua value AST. `expected` is an optional type-AST hint, used
    only to disambiguate int vs nat (a non-negative int under a `nat` type encodes as nat) and to pass
    element-type hints into lists. Raises ValueEncodeError for values with no value-AST representation.
    """
    # bool must precede int — bool is a subclass of int in Python.
    if isinstance(value, bool):
        return {"kind": "bool", "value": value}
    if isinstance(value, int):
        if _is_nat(expected) and value >= 0:
            return {"kind": "nat", "value": _int_lit(value)}
        return {"kind": "int", "value": _int_lit(value)}
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueEncodeError("non-finite float (NaN/Inf) has no canonical value form")
        return {"kind": "float", "value": value}
    if isinstance(value, str):
        return {"kind": "string", "value": value}
    if isinstance(value, (bytes, bytearray)):
        return {"kind": "bytes", "value": base64.b64encode(bytes(value)).decode("ascii")}
    if value is None:
        return {"kind": "unit"}
    if isinstance(value, list):
        elem = _ctor_arg(expected, "List")
        return {"kind": "list", "elems": [to_value_ast(v, elem) for v in value]}
    if isinstance(value, tuple):
        if len(value) == 0:
            return {"kind": "unit"}              # the empty tuple is unit (value schema)
        if len(value) == 1:
            return to_value_ast(value[0])        # a 1-element value is just the element
        return {"kind": "tuple", "elems": [to_value_ast(v) for v in value]}
    if isinstance(value, dict):
        fields = []
        for key, val in value.items():
            if not (isinstance(key, str) and _FIELD.match(key)):
                raise ValueEncodeError(
                    f"dict key {key!r} is not a lowercase-identifier field name (no Map value kind)")
            fields.append({"name": key, "value": to_value_ast(val)})
        return {"kind": "record", "fields": fields}
    raise ValueEncodeError(f"no value AST for a Python {type(value).__name__}")
