#!/usr/bin/env python3
"""Extract REAL worked examples from Python doctests, as Nova Lingua value ASTs.

This is the safe, execution-free half of example enrichment (the gap that blocks adapter-lifted draft
records from becoming complete v0.2 records — which require >=1 worked example as value ASTs). It
parses `>>> func(<literal args>)` calls and their literal expected output from a docstring, evaluates
ONLY the literals (`ast.literal_eval`) — never the function itself — and encodes inputs/outputs as
value ASTs via `nl_values`. A doctest whose args/output aren't literals, or aren't representable as
value ASTs, is skipped. The examples are author-provided and real; nothing is executed or fabricated.

(Execution-based generation — synthesising inputs from a type and running pure functions to capture
outputs — is a planned follow-on for functions that lack doctests.)

    python3 nl_examples.py path/to/module.py        # print {func: [{args, result}, ...]} as JSON
"""

import ast
import doctest

from nl_values import ValueEncodeError, to_value_ast


def examples_from_docstring(func_name, docstring, param_types=None, result_type=None, limit=8):
    """Return [{"args": [valueAST, ...], "result": valueAST}, ...] from the docstring's doctests that
    call `func_name` with literal positional args and a literal expected result. `param_types` /
    `result_type` are optional type-AST hints (int vs nat, list element types)."""
    if not docstring:
        return []
    try:
        items = doctest.DocTestParser().get_examples(docstring)
    except ValueError:
        return []
    out = []
    for ex in items:
        one = _example(func_name, ex.source, ex.want, param_types, result_type)
        if one is not None:
            out.append(one)
            if len(out) >= limit:
                break
    return out


def _example(func_name, source, want, param_types, result_type):
    try:
        node = ast.parse(source.strip(), mode="eval").body
    except SyntaxError:
        return None
    if not isinstance(node, ast.Call) or node.keywords:        # positional-only calls
        return None
    callee = node.func
    name = callee.attr if isinstance(callee, ast.Attribute) else getattr(callee, "id", None)
    if name != func_name:
        return None

    args = []
    for i, arg in enumerate(node.args):
        try:
            pyval = ast.literal_eval(arg)                      # literals only — never executes code
        except (ValueError, SyntaxError, TypeError):
            return None
        hint = param_types[i] if param_types and i < len(param_types) else None
        try:
            args.append(to_value_ast(pyval, hint))
        except ValueEncodeError:
            return None

    want = want.strip()
    if not want:
        return None
    # A `Traceback` doctest is exactly the missing doctest form for a None result: under a
    # Maybe-typed result (a raise-totalized function — the adapter wrapped its result type),
    # the raising example IS the None-case example, runnable like any other.
    if want.startswith("Traceback") and _is_maybe_type(result_type):
        return {"args": args, "result": {"kind": "variant", "tag": "None"}}
    try:
        result_py = ast.literal_eval(want)
    except (ValueError, SyntaxError, TypeError):
        return None
    try:
        result = to_value_ast(result_py, result_type)
    except ValueEncodeError:
        return None
    return {"args": args, "result": result}


def _is_maybe_type(t):
    return (isinstance(t, dict) and t.get("kind") == "apply"
            and (t.get("ctor") or {}).get("name") == "Maybe")


def module_examples(path):
    """{public_func_name: [examples]} for every top-level function in a .py file with usable doctests."""
    tree = ast.parse(open(path, encoding="utf-8").read())
    out = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.name.startswith("_"):
            exs = examples_from_docstring(node.name, ast.get_docstring(node))
            if exs:
                out[node.name] = exs
    return out


if __name__ == "__main__":
    import json
    import sys
    print(json.dumps(module_examples(sys.argv[1]), indent=2))
