"""Tests for conservative effect & termination inference (nl_effects).

Inference is a documented LOWER BOUND: these tests assert that recognisable effects ARE detected
and that the termination classes are conservative, not that the absence of an effect proves purity.
"""

import ast
import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-common
from nl_effects import (  # noqa: E402
    effects_from_py,
    terminates_from_py,
    effects_from_tokens,
    terminates_from_tokens,
)


def _fn(src):
    """Parse a module of imports + a single def; return (funcdef, alias, fromimp)."""
    tree = ast.parse(src)
    alias, fromimp = {}, {}
    func = None
    for node in tree.body:
        if isinstance(node, ast.Import):
            for n in node.names:
                if n.asname:
                    alias[n.asname] = n.name
                else:
                    root = n.name.split(".")[0]
                    alias[root] = root
        elif isinstance(node, ast.ImportFrom) and node.module:
            for n in node.names:
                fromimp[n.asname or n.name] = node.module
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            func = node
    return func, alias, fromimp


def eff(src):
    func, alias, fromimp = _fn(src)
    return effects_from_py(func, alias, fromimp)


def term(src):
    func, _, _ = _fn(src)
    return terminates_from_py(func)


class PythonEffectTests(unittest.TestCase):
    def test_open_modes(self):
        self.assertEqual(eff("def f(p):\n    return open(p).read()"), ["fs.read"])
        self.assertEqual(eff("def f(p):\n    return open(p, 'w')"), ["fs.write"])
        self.assertEqual(sorted(eff("def f(p):\n    return open(p, 'r+')")), ["fs.read", "fs.write"])

    def test_qualified_calls(self):
        self.assertEqual(eff("import socket\ndef f(s):\n    return s.recv(10)"), [])  # method on unknown receiver
        self.assertEqual(eff("import socket\ndef f():\n    return socket.recv(1)"), ["net.read"])
        self.assertEqual(eff("import time\ndef f():\n    return time.time()"), ["time"])
        self.assertEqual(eff("import random\ndef f():\n    return random.random()"), ["random"])
        self.assertEqual(eff("import subprocess\ndef f():\n    subprocess.run(['ls'])"), ["process.spawn"])

    def test_from_import_resolution(self):
        self.assertEqual(eff("from requests import get\ndef f(u):\n    return get(u)"), ["net.read"])
        self.assertEqual(eff("import numpy as np\ndef f():\n    return np.random.rand()"), ["random"])

    def test_print_and_raise(self):
        self.assertEqual(eff("def f(x):\n    print(x)"), ["io.console"])
        self.assertEqual(eff("def f(x):\n    raise ValueError(x)"), ["panic"])

    def test_leading_assert_is_not_panic(self):
        # A leading precondition assert is a contract guard, not a runtime panic effect.
        self.assertEqual(eff("def f(n):\n    assert n > 0\n    return n"), [])
        # A non-leading assert does count.
        self.assertEqual(eff("def f(n):\n    x = n\n    assert x > 0\n    return x"), ["panic"])

    def test_nested_function_effects_excluded(self):
        src = "def f():\n    def g():\n        print('hi')\n    return 1"
        self.assertEqual(eff(src), [])  # g is defined, not called


class PythonTerminationTests(unittest.TestCase):
    def test_straight_line_is_always(self):
        self.assertEqual(term("def f(a, b):\n    return a + b"), "always")
        self.assertEqual(term("def f(xs):\n    return len(xs)"), "always")  # len is a total builtin

    def test_self_recursion_is_conditional(self):
        self.assertEqual(term("def f(n):\n    return 1 if n == 0 else n * f(n - 1)"), "conditional")

    def test_loops_and_opaque_calls_are_unknown(self):
        self.assertEqual(term("def f(xs):\n    t = 0\n    for x in xs:\n        t += x\n    return t"), "unknown")
        self.assertEqual(term("def f(n):\n    return helper(n)"), "unknown")
        self.assertEqual(term("def f(xs):\n    return [x for x in xs]"), "unknown")  # comprehension


class TokenScanTests(unittest.TestCase):
    def test_haskell_tokens(self):
        self.assertEqual(effects_from_tokens("f x = readFile x", "hs"), ["fs.read"])
        self.assertEqual(effects_from_tokens("f x = putStrLn (show x)", "hs"), ["io.console"])
        self.assertEqual(effects_from_tokens("f = error \"boom\"", "hs"), ["panic"])

    def test_typescript_tokens(self):
        self.assertEqual(effects_from_tokens("const f = (u) => fetch(u)", "ts"), ["net.read", "net.write"])
        self.assertEqual(effects_from_tokens("const r = () => Math.random()", "ts"), ["random"])
        self.assertEqual(effects_from_tokens("const log = (x) => console.log(x)", "ts"), ["io.console"])

    def test_token_word_boundary(self):
        # `fetchData` must NOT match the `fetch` token.
        self.assertEqual(effects_from_tokens("const f = () => fetchData()", "ts"), [])

    def test_token_termination_recursion(self):
        self.assertEqual(terminates_from_tokens("fact", "fact n = if n == 0 then 1 else n * fact (n-1)", "hs"), "conditional")
        self.assertEqual(terminates_from_tokens("inc", "inc n = n + 1", "hs"), "unknown")


if __name__ == "__main__":
    unittest.main()
