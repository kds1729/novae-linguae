"""Tests for the pragmatic body-expression AST builder (nl_body), incl. cross-validation against
nl-validator check-body (well-formedness) and hash (the body content-address)."""

import ast
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-common
from nl_body import body_ast_from_py, body_ast_from_hs, body_ast_from_ts  # noqa: E402
from nl_core import blake3_256, canonicalize, format_hash  # noqa: E402

VALIDATOR = _HERE.parents[2] / "validator" / "target" / "release" / "nl-validator"


def py_body(src):
    return body_ast_from_py(ast.parse(src).body[0])


class PythonBodyTests(unittest.TestCase):
    def test_arithmetic_expression_wraps_in_lambda(self):
        self.assertEqual(
            py_body("def f(n):\n    return n * 2"),
            {"kind": "lambda", "params": [{"name": "n"}], "body":
                {"kind": "app", "fn": {"kind": "var", "name": "mul"},
                 "args": [{"kind": "var", "name": "n"}, {"kind": "lit", "value": {"kind": "int", "value": 2}}]}},
        )

    def test_var_and_call(self):
        self.assertEqual(
            py_body("def f(x):\n    return x"),
            {"kind": "lambda", "params": [{"name": "x"}], "body": {"kind": "var", "name": "x"}},
        )
        self.assertEqual(
            py_body("def f(x):\n    return g(x)"),
            {"kind": "lambda", "params": [{"name": "x"}], "body":
                {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]}},
        )

    def test_zero_arg_is_a_bare_expression(self):
        # A 0-param function emits no lambda — applying it to [] still evaluates.
        self.assertEqual(py_body("def f():\n    return 1"), {"kind": "lit", "value": {"kind": "int", "value": 1}})

    def test_docstring_is_skipped(self):
        self.assertEqual(
            py_body('def f(x):\n    "doc"\n    return x'),
            {"kind": "lambda", "params": [{"name": "x"}], "body": {"kind": "var", "name": "x"}},
        )

    def test_local_binding_becomes_let(self):
        self.assertEqual(
            py_body("def f(x):\n    y = x\n    return y"),
            {"kind": "lambda", "params": [{"name": "x"}], "body":
                {"kind": "let", "name": "y", "value": {"kind": "var", "name": "x"},
                 "body": {"kind": "var", "name": "y"}}},
        )

    def test_boolean_if_becomes_case(self):
        body = py_body("def f(n):\n    if n > 0:\n        return 1\n    return 0")
        self.assertEqual(body["kind"], "lambda")
        case = body["body"]
        self.assertEqual(case["kind"], "case")
        self.assertEqual(
            case["scrutinee"],
            {"kind": "app", "fn": {"kind": "var", "name": "gt"},
             "args": [{"kind": "var", "name": "n"}, {"kind": "lit", "value": {"kind": "int", "value": 0}}]},
        )
        self.assertEqual(case["arms"][0]["pattern"], {"kind": "lit", "value": {"kind": "bool", "value": True}})
        self.assertEqual(case["arms"][1]["pattern"], {"kind": "wildcard"})

    def test_ternary_becomes_case(self):
        self.assertEqual(py_body("def f(n):\n    return 1 if n > 0 else 0")["body"]["kind"], "case")

    def test_len_maps_to_length(self):
        self.assertEqual(
            py_body("def f(xs):\n    return len(xs)")["body"],
            {"kind": "app", "fn": {"kind": "var", "name": "length"}, "args": [{"kind": "var", "name": "xs"}]},
        )

    def test_list_comprehension_becomes_map(self):
        body = py_body("def f(xs):\n    return [x * 2 for x in xs]")
        self.assertEqual(body["body"]["fn"], {"kind": "var", "name": "map"})

    def test_filtered_comprehension_uses_filter(self):
        # `[x for x in xs if x > 0]` is identity-over-filter -> just filter(\x -> gt(x,0), xs).
        body = py_body("def f(xs):\n    return [x for x in xs if x > 0]")
        self.assertEqual(body["body"]["fn"], {"kind": "var", "name": "filter"})

    def test_accumulator_loop_becomes_foldl(self):
        body = py_body("def f(xs):\n    acc = 0\n    for x in xs:\n        acc = acc + x\n    return acc")
        inner = body["body"]["body"]  # lambda -> (let acc=0 in <let acc=foldl(...) in acc>)
        self.assertEqual(inner["value"]["fn"], {"kind": "var", "name": "foldl"})

    def test_augmented_accumulator_loop_becomes_foldl(self):
        # `acc += x` is the common idiom — equivalent to the explicit `acc = acc + x` form.
        aug = py_body("def f(xs):\n    acc = 0\n    for x in xs:\n        acc += x\n    return acc")
        plain = py_body("def f(xs):\n    acc = 0\n    for x in xs:\n        acc = acc + x\n    return acc")
        self.assertEqual(aug, plain)
        prod = py_body("def f(xs):\n    acc = 1\n    for x in xs:\n        acc *= x\n    return acc")
        fold = prod["body"]["body"]["value"]
        self.assertEqual(fold["fn"], {"kind": "var", "name": "foldl"})
        # update lambda body is `mul(acc, x)`
        self.assertEqual(fold["args"][0]["body"]["fn"], {"kind": "var", "name": "mul"})

    def test_top_level_augmented_assignment_becomes_let(self):
        # `n += 1` outside a loop re-binds n to add(n, 1).
        body = py_body("def f(n):\n    n += 1\n    return n")
        let = body["body"]  # lambda -> let n = add(n,1) in n
        self.assertEqual(let["kind"], "let")
        self.assertEqual(let["value"]["fn"], {"kind": "var", "name": "add"})

    def test_out_of_subset_returns_none(self):
        self.assertIsNone(py_body("def f(x):\n    if x:\n        return 1\n    return 0"))  # truthy non-bool test
        self.assertIsNone(py_body("def f(x):\n    return [i for r in x for i in r]"))  # multi-generator comp
        self.assertIsNone(py_body("def f(x):\n    return"))  # bare return
        self.assertIsNone(py_body("def f(x):\n    while x > 0:\n        x -= 1\n    return x"))  # while (non-structural)
        # Loop variable read AFTER a search loop (Python: last element; the translation: unbound).
        self.assertIsNone(py_body("def f(x):\n    for i in x:\n        if i > 0:\n            return i\n    return i"))

    def test_floordiv_maps_to_div(self):
        # `//` -> the same Euclidean `div` as `/` (they agree for positive divisors; a wrong
        # guess fails the example gate — the `%` -> mod contract).
        self.assertIn('"div"', json.dumps(py_body("def f(a, b):\n    return a // b")))
        aug = py_body("def f(n):\n    n //= 2\n    return n")
        self.assertIn('"div"', json.dumps(aug))

    def test_multi_accumulator_loop_splits_into_folds(self):
        # Independent accumulator statements split into one fold each — exact in a pure total
        # language (re-walking the list is unobservable), like the search loop's short-circuit.
        body = py_body("def f(xs):\n    s = 0\n    c = 0\n"
                       "    for x in xs:\n        s += x\n        c += 1\n    return s - c")
        s = json.dumps(body)
        self.assertEqual(s.count('"foldl"'), 2)
        # Source order of the accumulators is kept: s's fold binds outermost.
        self.assertLess(s.index('"name": "s"'), s.index('"name": "c"'))

    def test_dependent_accumulators_are_out_of_subset(self):
        # `c += s` reads s's MID-LOOP value — a separate fold can't reproduce it; refused.
        self.assertIsNone(py_body("def f(xs):\n    s = 0\n    c = 0\n"
                                  "    for x in xs:\n        s += x\n        c += s\n    return c"))
        # A guard reading an accumulator has the same mid-loop problem.
        self.assertIsNone(py_body("def f(xs):\n    s = 0\n    c = 0\n"
                                  "    for x in xs:\n        if x > c:\n            s += x\n"
                                  "            c += 1\n    return s"))
        # The same accumulator twice is sequential by construction.
        self.assertIsNone(py_body("def f(xs):\n    s = 0\n"
                                  "    for x in xs:\n        s += x\n        s += 1\n    return s"))

    def test_nested_list_building_loop_becomes_fold_of_appends(self):
        # flatten: `for row in xss: for i in row: out.append(i)` -> foldl of per-row appends,
        # seeded with out's prior value.
        body = py_body("def f(xss):\n    out = []\n"
                       "    for row in xss:\n        for i in row:\n            out.append(i)\n"
                       "    return out")
        s = json.dumps(body)
        self.assertIn('"foldl"', s)
        self.assertIn('"append"', s)
        # An inner guard filters the row's batch.
        guarded = py_body("def f(xss):\n    out = []\n"
                          "    for row in xss:\n        for i in row:\n"
                          "            if i > 0:\n                out.append(i)\n    return out")
        self.assertIn('"filter"', json.dumps(guarded))

    def test_nested_loop_reading_accumulator_is_out_of_subset(self):
        # The element expression reading `out` mid-loop sees Python's growing list — a fold step
        # can't reproduce that; refused.
        self.assertIsNone(py_body("def f(xss):\n    out = []\n"
                                  "    for row in xss:\n        for i in row:\n"
                                  "            out.append(len(out))\n    return out"))

    def test_loop_var_read_after_any_loop_is_out_of_subset(self):
        # The read-after-loop honesty guard covers the accumulator/append shapes too (Python
        # leaves the loop variable bound to the last element; the translations do not).
        self.assertIsNone(py_body("def f(xs):\n    s = 0\n    for x in xs:\n        s += x\n    return x"))
        self.assertIsNone(py_body("def f(xs):\n    out = []\n    for x in xs:\n        out.append(x)\n    return x"))

    def test_is_none_narrowing_becomes_case(self):
        # `if x is None:` over an Optional param -> a case on the Maybe; the non-None branch
        # REBINDS x to the Just payload (Python's narrowing made explicit).
        body = py_body("def f(x: int | None, d):\n    if x is None:\n        return d\n    return x")
        case = body["body"]
        self.assertEqual(case["kind"], "case")
        self.assertEqual(case["scrutinee"], {"kind": "var", "name": "x"})
        just = next(a for a in case["arms"] if a["pattern"].get("tag") == "Just")
        self.assertEqual(just["pattern"]["payload"], {"kind": "bind", "name": "x"})
        self.assertEqual(just["body"], {"kind": "var", "name": "x"})
        # `is not None` narrows symmetrically.
        b2 = py_body("def f(x: int | None):\n    if x is not None:\n        return x + 1\n    return 0")
        just2 = next(a for a in b2["body"]["arms"] if a["pattern"].get("tag") == "Just")
        self.assertEqual(just2["body"]["fn"], {"kind": "var", "name": "add"})

    def test_none_branch_reading_narrowed_name_is_refused(self):
        self.assertIsNone(py_body(
            "def f(x: int | None):\n    if x is None:\n        return x\n    return x"))

    def test_optional_return_wraps(self):
        # In an `-> Optional[T]` function: `return None` -> the None variant, a plain value ->
        # Just(...), an already-Maybe expression (bare 1-arg get) passes through unwrapped.
        b = py_body("def f(x: int | None) -> int | None:\n"
                    "    if x is None:\n        return None\n    return x + 1")
        s = json.dumps(b)
        self.assertIn('{"kind": "variant", "tag": "None"}', s)
        self.assertIn('"tag": "Just"', s)
        passthrough = py_body('def f(d: dict, k) -> int | None:\n    return d.get(k)')
        self.assertEqual(passthrough["body"]["fn"], {"kind": "var", "name": "map_get"})
        # Without the return annotation nothing wraps (hash stability for plain functions).
        plain = py_body("def f(n):\n    return n + 1")
        self.assertNotIn('"variant"', json.dumps(plain))

    def test_maybe_returning_search_loop(self):
        # The flagship composition: a search loop in an Optional function — the hit wraps in
        # Just, the not-found `return None` is the None variant.
        b = py_body("def f(xs, c) -> int | None:\n"
                    "    for x in xs:\n        if x > c:\n            return x\n    return None")
        s = json.dumps(b)
        self.assertIn('"filter"', s)
        self.assertIn('"tag": "Just"', s)
        self.assertIn('{"kind": "variant", "tag": "None"}', s)

    def test_search_loop_becomes_filter_head(self):
        # `for i in x: return i` was the old subset boundary; it is now the degenerate search —
        # head-or-default — and the guarded form filters first (exact in a pure total language).
        body = py_body("def f(x):\n    for i in x:\n        return i\n    return 0")
        let = body["body"]  # lambda -> let hits = x in case null(hits) of ...
        self.assertEqual(let["kind"], "let")
        self.assertEqual(let["value"], {"kind": "var", "name": "x"})
        self.assertEqual(let["body"]["scrutinee"]["fn"], {"kind": "var", "name": "null"})
        guarded = py_body("def f(x):\n    for i in x:\n        if i > 0:\n            return i\n    return 0")
        self.assertEqual(guarded["body"]["value"]["fn"], {"kind": "var", "name": "filter"})


class TokenBodyTests(unittest.TestCase):
    def test_haskell_bare_and_application_wrap_in_lambda(self):
        self.assertEqual(
            body_ast_from_hs("f", "f x = x"),
            {"kind": "lambda", "params": [{"name": "x"}], "body": {"kind": "var", "name": "x"}},
        )
        self.assertEqual(
            body_ast_from_hs("f", "f x = g x")["body"],
            {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]},
        )
        self.assertIsNone(body_ast_from_hs("f", "f x = x + 1"))         # operator: out of HS subset
        self.assertIsNone(body_ast_from_hs("f", "f x\n  | x > 0 = 1"))  # guard

    def test_typescript_arrow_reuses_python_expr(self):
        self.assertEqual(
            body_ast_from_ts("f", "export const f = (x) => x"),
            {"kind": "lambda", "params": [{"name": "x"}], "body": {"kind": "var", "name": "x"}},
        )
        self.assertEqual(
            body_ast_from_ts("f", "export const f = (x) => g(x)")["body"],
            {"kind": "app", "fn": {"kind": "var", "name": "g"}, "args": [{"kind": "var", "name": "x"}]},
        )
        # Operators now translate (TS expression syntax == Python here).
        self.assertEqual(
            body_ast_from_ts("f", "export const f = (x) => x * 2")["body"],
            {"kind": "app", "fn": {"kind": "var", "name": "mul"},
             "args": [{"kind": "var", "name": "x"}, {"kind": "lit", "value": {"kind": "int", "value": 2}}]},
        )
        self.assertEqual(  # multi-param + comparison
            body_ast_from_ts("f", "export const f = (a, b) => a > b")["params"],
            [{"name": "a"}, {"name": "b"}],
        )
        self.assertIsNone(body_ast_from_ts("f", "export function f(x) { return x; }"))  # block body


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class CrossValidationTests(unittest.TestCase):
    def _run(self, args, payload):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(payload, fh)
            path = fh.name
        return subprocess.run([str(VALIDATOR), *args, path], capture_output=True, text=True)

    def test_body_is_well_formed_and_hash_matches_validator(self):
        body = py_body("def f(n):\n    return g(n, 2)")
        cb = self._run(["check-body"], body)
        self.assertEqual(cb.returncode, 0, cb.stderr)
        # The address we compute equals the validator's body content-address.
        ours = format_hash("expr", blake3_256(canonicalize(body)))
        got = self._run(["hash"], body)
        self.assertEqual(got.returncode, 0, got.stderr)
        self.assertEqual(got.stdout.strip(), ours)


if __name__ == "__main__":
    unittest.main()
