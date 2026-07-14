"""Structured type matching for `/v0/query` — the `type_pattern` filter (spec/commons.md).

`type_contains` is a substring hint over the *rendering* of a type; this module matches a structured
PATTERN against the stored v0.2 type AST (spec/type-expression.schema.json) by unification — the
operable form of open question 5 ("query over structured ASTs"). Matching is exact and node-portable
like the rest of typed query: it is computed from the record's own `signature.type`, so any correct
node returns the same set for the same corpus.

Pattern grammar — every node of the type-expression AST, plus three pattern-only forms:

  {"kind": "any"}                      wildcard: matches any type
  {"kind": "any_of", "types": [...]}   disjunction: matches if any branch matches
  {"kind": "head", "names": [...]}     the type's HEAD constructor is a builtin in the set
                                       (bare `builtin` or the ctor of an `apply`)

Semantics:
  • A `var` in the PATTERN is a named wildcard with consistency: every occurrence must match the
    same record-side subtree (so `{a} -> {a}` finds `forall b. b -> b` but not `int -> string`).
  • A `var` in the RECORD type is a unification variable (canonical types bind them by `forall`):
    it matches any pattern subtree, consistently across occurrences.
  • `forall` is stripped on both sides before matching (rank-1 only, per the schema).
  • Builtin names match EXACTLY: a pattern `int` does not match a declared `nat` — a caller who
    accepts either says so with `any_of` (or `head`). Structural forms (`fn`, `apply`, `tuple`,
    `record`, `sum`, `ref`) match structurally: same shape, same arity/fields/tags, subtrees match.
  • Only records with a STRUCTURED type participate: a v0.1 string-typed record never matches a
    `type_pattern` (use `type_contains` for those).

The matcher backtracks through `any_of` by branching the two substitutions, and errs PERMISSIVE when
two pattern subtrees bound to the same record variable cannot be precisely intersected — a discovery
filter must never silently hide a usable function (under-rejection is caught by re-verification).
"""

import json


class PatternError(ValueError):
    """A malformed `type_pattern` — query.validate_filter re-raises it as a QueryError (HTTP 400)."""


_PATTERN_KINDS = {"any", "any_of", "head", "var", "ref", "builtin", "forall", "fn", "apply",
                  "tuple", "record", "sum"}


def validate_pattern(p, path="type_pattern"):
    """Raise PatternError unless `p` is a well-shaped pattern. Shape-checks only — builtin names are
    NOT checked against the closed enum, so a pattern written for a newer type vocabulary is a
    non-match on this node, not a 400."""
    if not isinstance(p, dict):
        raise PatternError(f"`{path}` must be an object (a type pattern), not {type(p).__name__}")
    kind = p.get("kind")
    if kind not in _PATTERN_KINDS:
        raise PatternError(f"`{path}.kind` must be one of {sorted(_PATTERN_KINDS)}, got {kind!r}")
    if kind == "any":
        return p
    if kind == "any_of":
        branches = p.get("types")
        if not isinstance(branches, list) or not branches:
            raise PatternError(f"`{path}.types` must be a non-empty array of patterns")
        for i, b in enumerate(branches):
            validate_pattern(b, f"{path}.types[{i}]")
    elif kind == "head":
        names = p.get("names")
        if (not isinstance(names, list) or not names
                or not all(isinstance(n, str) and n for n in names)):
            raise PatternError(f"`{path}.names` must be a non-empty array of builtin names")
    elif kind == "var":
        if not isinstance(p.get("name"), str) or not p["name"]:
            raise PatternError(f"`{path}.name` must be a non-empty string")
    elif kind == "ref":
        if not isinstance(p.get("target"), str):
            raise PatternError(f"`{path}.target` must be a string")
    elif kind == "builtin":
        if not isinstance(p.get("name"), str) or not p["name"]:
            raise PatternError(f"`{path}.name` must be a non-empty string")
    elif kind == "forall":
        validate_pattern(p.get("body"), f"{path}.body")
    elif kind == "fn":
        params = p.get("params")
        if not isinstance(params, list):
            raise PatternError(f"`{path}.params` must be an array of patterns")
        for i, q in enumerate(params):
            validate_pattern(q, f"{path}.params[{i}]")
        validate_pattern(p.get("result"), f"{path}.result")
    elif kind == "apply":
        validate_pattern(p.get("ctor"), f"{path}.ctor")
        args = p.get("args")
        if not isinstance(args, list) or not args:
            raise PatternError(f"`{path}.args` must be a non-empty array of patterns")
        for i, q in enumerate(args):
            validate_pattern(q, f"{path}.args[{i}]")
    elif kind == "tuple":
        elems = p.get("elems")
        if not isinstance(elems, list) or len(elems) < 2:
            raise PatternError(f"`{path}.elems` must be an array of ≥2 patterns")
        for i, q in enumerate(elems):
            validate_pattern(q, f"{path}.elems[{i}]")
    elif kind == "record":
        fields = p.get("fields")
        if not isinstance(fields, list):
            raise PatternError(f"`{path}.fields` must be an array")
        for i, f in enumerate(fields):
            if not isinstance(f, dict) or not isinstance(f.get("name"), str):
                raise PatternError(f"`{path}.fields[{i}]` must be {{name, type}}")
            validate_pattern(f.get("type"), f"{path}.fields[{i}].type")
    elif kind == "sum":
        variants = p.get("variants")
        if not isinstance(variants, list) or not variants:
            raise PatternError(f"`{path}.variants` must be a non-empty array")
        for i, v in enumerate(variants):
            if not isinstance(v, dict) or not isinstance(v.get("tag"), str):
                raise PatternError(f"`{path}.variants[{i}]` must be {{tag[, type]}}")
            if "type" in v:
                validate_pattern(v["type"], f"{path}.variants[{i}].type")
    return p


def _strip_forall(t):
    while isinstance(t, dict) and t.get("kind") == "forall":
        t = t.get("body")
    return t


def _head_name(t):
    """The head builtin name of a record-side type, if it has one."""
    if t.get("kind") == "builtin":
        return t.get("name")
    if t.get("kind") == "apply":
        ctor = t.get("ctor", {})
        if isinstance(ctor, dict) and ctor.get("kind") == "builtin":
            return ctor.get("name")
    return None


def _compat(p, q):
    """Could patterns `p` and `q` admit a common type? Used when the same record variable is bound
    from two occurrences. Approximate, and deliberately errs PERMISSIVE on shapes it cannot decide
    (a filter must not over-reject)."""
    for a, b in ((p, q), (q, p)):
        if a.get("kind") == "any" or a.get("kind") == "var":
            return True
        if a.get("kind") == "any_of":
            return any(_compat(t, b) for t in a["types"])
    pk, qk = p.get("kind"), q.get("kind")
    if pk == "head" or qk == "head":
        if pk == "head" and qk == "head":
            return bool(set(p["names"]) & set(q["names"]))
        h, other = (p, q) if pk == "head" else (q, p)
        if other.get("kind") == "builtin":
            return other.get("name") in h["names"]
        if other.get("kind") == "apply":
            ctor = other.get("ctor", {})
            if isinstance(ctor, dict) and ctor.get("kind") == "builtin":
                return ctor.get("name") in h["names"]
            return True  # ctor is a var/pattern — can't decide, stay permissive
        return False
    if pk != qk:
        return False
    if pk == "builtin":
        return p.get("name") == q.get("name")
    if pk == "ref":
        return p.get("target") == q.get("target")
    if pk == "fn":
        return (len(p["params"]) == len(q["params"])
                and all(_compat(a, b) for a, b in zip(p["params"], q["params"]))
                and _compat(p["result"], q["result"]))
    if pk == "apply":
        return (len(p["args"]) == len(q["args"]) and _compat(p["ctor"], q["ctor"])
                and all(_compat(a, b) for a, b in zip(p["args"], q["args"])))
    if pk == "tuple":
        return (len(p["elems"]) == len(q["elems"])
                and all(_compat(a, b) for a, b in zip(p["elems"], q["elems"])))
    return True  # record/sum intersection: permissive


class _Resolver:
    """Bounded resolution of `ref` nodes through published `type_…` artifacts. `load(target)`
    returns the referenced type expression or None. A GLOBAL load budget bounds the whole match
    (a pathological web of mutually-referencing structural types cannot recurse unboundedly:
    every level costs one load, and an exhausted budget leaves a bare, unmatchable ref); the
    per-chain cycle set lives in [`_deref`], so the same legitimate ref may appear — and
    resolve — any number of times across the type."""

    BUDGET = 64

    def __init__(self, load):
        self.load = load
        self.remaining = self.BUDGET

    def resolve(self, target):
        if self.load is None or self.remaining <= 0:
            return None
        self.remaining -= 1
        got = self.load(target)
        return got if isinstance(got, dict) else None


def _deref(node, res):
    """Follow a CHAIN of `ref` nodes (and strip foralls) until a structural node, a variable, or
    an unresolvable ref remains. The chain's own targets form the cycle guard (a ref chain that
    revisits a target can never bottom out)."""
    node = _strip_forall(node)
    chain = set()
    while isinstance(node, dict) and node.get("kind") == "ref" and res is not None:
        target = node.get("target", "")
        if target in chain:
            return node  # a cyclic alias chain has no structural definition
        resolved = res.resolve(target)
        if resolved is None:
            return node  # absent on this node, or the global budget is spent
        chain.add(target)
        node = _strip_forall(resolved)
    return node


def _match(p, t, psub, rsub, res=None):
    """Match pattern `p` against record-side type `t`, threading pattern-variable bindings (`psub`,
    var name → record subtree) and record-variable bindings (`rsub`, var name → pattern subtree).
    Mutates the substitutions on success; `any_of` branches on copies. `res` (a [`_Resolver`] or
    None) lets `ref` nodes on EITHER side match through their published definitions; an
    unresolvable ref matches only `any`, a pattern variable, or an identical ref."""
    p = _strip_forall(p)
    t = _strip_forall(t)
    if not isinstance(p, dict) or not isinstance(t, dict):
        return False
    pk = p.get("kind")
    if pk == "any":
        return True
    if pk == "any_of":
        for branch in p["types"]:
            ps, rs = dict(psub), dict(rsub)
            if _match(branch, t, ps, rs, res):
                psub.clear(); psub.update(ps)
                rsub.clear(); rsub.update(rs)
                return True
        return False
    # A record-side variable unifies with any pattern, consistently across occurrences.
    if t.get("kind") == "var":
        name = t.get("name", "")
        if name in rsub:
            return _compat(rsub[name], p)
        rsub[name] = p
        return True
    if pk == "var":
        name = p["name"]
        if name in psub:
            return psub[name] == t  # canonical ASTs: structural equality is alpha-consistent here
        psub[name] = t
        return True
    # Identical refs match without resolving (content-addresses: same target = same definition).
    if pk == "ref" and t.get("kind") == "ref" and p.get("target") == t.get("target"):
        return True
    # Otherwise a ref on either side matches through its published definition, when resolvable.
    if pk == "ref" or t.get("kind") == "ref":
        p2, t2 = _deref(p, res), _deref(t, res)
        if (isinstance(p2, dict) and p2.get("kind") == "ref") or \
           (isinstance(t2, dict) and t2.get("kind") == "ref"):
            return False  # an unresolvable ref cannot be structurally matched
        return _match(p2, t2, psub, rsub, res)
    if pk == "head":
        return _head_name(t) in p["names"]
    tk = t.get("kind")
    if pk != tk:
        return False
    if pk == "builtin":
        return p.get("name") == t.get("name")
    if pk == "fn":
        tparams = t.get("params", [])
        if len(p["params"]) != len(tparams):
            return False
        return (all(_match(a, b, psub, rsub, res) for a, b in zip(p["params"], tparams))
                and _match(p["result"], t.get("result", {}), psub, rsub, res))
    if pk == "apply":
        targs = t.get("args", [])
        if len(p["args"]) != len(targs):
            return False
        return (_match(p["ctor"], t.get("ctor", {}), psub, rsub, res)
                and all(_match(a, b, psub, rsub, res) for a, b in zip(p["args"], targs)))
    if pk == "tuple":
        telems = t.get("elems", [])
        if len(p["elems"]) != len(telems):
            return False
        return all(_match(a, b, psub, rsub, res) for a, b in zip(p["elems"], telems))
    if pk == "record":
        tfields = {f.get("name"): f.get("type") for f in t.get("fields", [])}
        pfields = {f["name"]: f["type"] for f in p["fields"]}
        if set(pfields) != set(tfields):
            return False
        return all(_match(pfields[n], tfields[n], psub, rsub, res) for n in pfields)
    if pk == "sum":
        tvariants = {v.get("tag"): v.get("type") for v in t.get("variants", [])}
        pvariants = {v["tag"]: v.get("type") for v in p["variants"]}
        if set(pvariants) != set(tvariants):
            return False
        for tag, pt in pvariants.items():
            tt = tvariants[tag]
            if (pt is None) != (tt is None):
                return False
            if pt is not None and not _match(pt, tt, psub, rsub, res):
                return False
        return True
    return False


def matches_type(pattern, type_str, load_type=None):
    """Does `pattern` match the stored `type_str`? `type_str` is the extracted column — the JSON
    rendering of a structured v0.2 type, or a v0.1 surface string (which never matches — structured
    matching needs a structured type). `load_type(target)` — when given — resolves `ref` nodes
    through published `type_…` artifacts (bounded depth, cycle-guarded), so a nominally-typed
    record matches a structural pattern and vice versa."""
    if not type_str:
        return False
    try:
        t = json.loads(type_str)
    except (ValueError, TypeError):
        return False
    if not isinstance(t, dict):
        return False
    return _match(pattern, t, {}, {}, _Resolver(load_type) if load_type else None)
