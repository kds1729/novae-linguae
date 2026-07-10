"""Tests for nl-ingest-ts.

    python3 -m unittest discover -s tooling/ingest-npm/tests

Cross-validation against the Rust nl-validator is skipped if the release binary isn't built.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOL_DIR = HERE.parent
REPO_ROOT = TOOL_DIR.parent.parent
SPEC_DIR = REPO_ROOT / "spec"
VALIDATOR = REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"
FR_SCHEMA = SPEC_DIR / "function-record.schema.json"
SAMPLE = HERE / "sample.ts"

sys.path.insert(0, str(TOOL_DIR))
sys.path.insert(0, str(REPO_ROOT / "tooling" / "ingest-common"))
import nl_ingest_ts as t  # noqa: E402
import nl_core as c  # noqa: E402


class TestCore(unittest.TestCase):
    def test_blake3_reference(self):
        self.assertEqual(c.blake3_256_pure(b"").hex(),
                         "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")


class TestCommentStripping(unittest.TestCase):
    def test_line_and_block(self):
        out = t.strip_comments("const a = 1; // x\n/* y */ const b = 2;")
        self.assertNotIn("x", out)
        self.assertNotIn("y", out)

    def test_comment_markers_in_strings_preserved(self):
        src = 'const u = "http://example.com/*"; export function f(): void {}'
        out = t.strip_comments(src)
        self.assertIn("http://example.com/*", out)
        self.assertIn("function f", out)


class TestParsing(unittest.TestCase):
    def _by_name(self, src):
        return {(r["name_hints"][0] if r["name_hints"] else "<anon>"): r
                for r in t.records_from_source(src)}

    def test_plain_function(self):
        r = self._by_name("export function add(a: number, b: number): number { return a+b; }")["add"]
        self.assertEqual(r["signature"]["type"], "(number, number) -> number")
        self.assertEqual(len(r["examples"][0]["args"]), 2)

    def test_generics_and_array_and_arrow_param(self):
        r = self._by_name("export function map<T, U>(xs: T[], f: (x: T) => U): U[] { return xs.map(f); }")["map"]
        self.assertEqual(r["signature"]["type"], "forall T U. (T[], (x: T) => U) -> U[]")
        self.assertEqual(len(r["examples"][0]["args"]), 2)  # arrow-type comma not split

    def test_generic_map_param_comma_not_split(self):
        r = self._by_name("export async function f(h: Map<string, string>): Promise<void> {}")["f"]
        self.assertEqual(r["signature"]["type"], "(Map<string, string>) -> Promise<void>")
        self.assertEqual(len(r["examples"][0]["args"]), 1)

    def test_arrow_const_with_return_type(self):
        r = self._by_name("export const toUpper = (s: string): string => s.toUpperCase();")["toupper"]
        self.assertEqual(r["signature"]["type"], "(string) -> string")

    def test_arrow_generic_optional_rest_arity(self):
        src = "export const pick = <T>(obj: T, key?: string, ...rest: string[]): unknown => obj;"
        r = self._by_name(src)["pick"]
        self.assertEqual(r["signature"]["type"], "forall T. (T, string, string[]) -> unknown")
        self.assertEqual(len(r["examples"][0]["args"]), 3)

    def test_function_expression_binding_name(self):
        r = self._by_name("export const negate = function (n: number): number { return -n; };")["negate"]
        self.assertEqual(r["signature"]["type"], "(number) -> number")

    def test_bare_identifier_arrow(self):
        r = self._by_name("export const identity = x => x;")["identity"]
        self.assertEqual(r["signature"]["type"], "(unknown) -> unknown")
        self.assertEqual(len(r["examples"][0]["args"]), 1)

    def test_ambient_declaration(self):
        r = self._by_name("export declare function parseConfig(text: string): Record<string, number>;")["parseconfig"]
        self.assertEqual(r["signature"]["type"], "(string) -> Record<string, number>")

    def test_default_anonymous(self):
        r = self._by_name("export default function (n: number): boolean { return n>0; }")["default"]
        self.assertEqual(r["signature"]["type"], "(number) -> boolean")

    def test_this_param_excluded(self):
        r = self._by_name("export function f(this: Ctx, a: number): void {}")["f"]
        self.assertEqual(r["signature"]["type"], "(number) -> void")
        self.assertEqual(len(r["examples"][0]["args"]), 1)

    def test_non_function_and_unexported_skipped(self):
        names = set(self._by_name(
            'export const VERSION = "1";\nfunction hidden(){}\nexport function shown(): void {}'))
        self.assertEqual(names, {"shown"})

    def test_module_hint(self):
        recs = t.records_from_source("export function f(): void {}", module_name="mypkg")
        self.assertIn("mypkg_f", recs[0]["name_hints"])


class TestRecordShape(unittest.TestCase):
    def test_hash_self_consistent(self):
        rec = t.records_from_source("export function f(a: number): number { return a; }")[0]
        self.assertEqual(c.content_hash(rec, "fn", strip=("hash",)), rec["hash"])
        self.assertTrue(rec["body_hash"].startswith("expr_"))


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestCrossValidation(unittest.TestCase):
    def _run(self, *args):
        return subprocess.run([str(VALIDATOR), *args], capture_output=True, text=True)

    def test_sample_records_validate_and_verify(self):
        recs = t.records_from_source(SAMPLE.read_text(), "sample")
        self.assertEqual(len(recs), 9)
        for rec in recs:
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
                json.dump(rec, f)
                path = f.name
            v = self._run("validate", str(FR_SCHEMA), path)
            self.assertEqual(v.returncode, 0, v.stderr)
            r = self._run("verify", path)
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_validator_hash_matches(self):
        rec = t.records_from_source(SAMPLE.read_text(), "sample")[0]
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
            path = f.name
        out = self._run("hash", path)
        self.assertEqual(out.stdout.strip(), rec["hash"])


class TestV2TypeMapping(unittest.TestCase):
    def test_containers_and_atomics(self):
        _ty, params, result = t.ts_function_type([], "xs: number[]", "string")
        self.assertEqual(params[0], {"kind": "apply", "ctor": {"kind": "builtin", "name": "List"},
                                     "args": [{"kind": "builtin", "name": "float"}]})
        self.assertEqual(result, {"kind": "builtin", "name": "string"})

    def test_optional_union_is_maybe(self):
        _ty, params, _r = t.ts_function_type([], "x: number | null", "")
        self.assertEqual(params[0], {"kind": "apply", "ctor": {"kind": "builtin", "name": "Maybe"},
                                     "args": [{"kind": "builtin", "name": "float"}]})


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TestV2Records(unittest.TestCase):
    FR_V2 = SPEC_DIR / "function-record.v0.2.schema.json"
    SRC = (
        "/**\n * @example\n * double(5) // => 10\n */\n"
        "export function double(n: number): number { return n * 2; }\n\n"
        "/** No example. */\nexport function noex(x: number): number { return x; }\n"
    )

    def _run(self, *a):
        return subprocess.run([str(VALIDATOR), *a], capture_output=True, text=True)

    def test_v2_from_jsdoc_example_and_fallback(self):
        recs = {r["name_hints"][0]: r for r in t.records_from_source(self.SRC, "demo", v2=True)}
        d = recs["double"]
        self.assertEqual(d["schema_version"], "0.2.0")
        self.assertEqual(d["signature"]["type"],
                         {"kind": "fn", "params": [{"kind": "builtin", "name": "float"}],
                          "result": {"kind": "builtin", "name": "float"}})
        self.assertEqual(d["examples"][0],
                         {"args": [{"kind": "float", "value": 5.0}],
                          "result": {"kind": "float", "value": 10.0}})
        self.assertEqual(recs["noex"]["schema_version"], "0.1.0")   # no @example -> v0.1 fallback

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(d, f)
            path = f.name
        self.assertEqual(self._run("validate", str(self.FR_V2), path).returncode, 0)
        self.assertEqual(self._run("verify", path).returncode, 0)

    def test_properties_flag_matches_on_arity(self):
        # Regression: arity passed to the catalog must be the parameter COUNT, not len(params-string).
        # `reverse/1` (data-first) matches the catalog; a 2-ary `reverse` must NOT.
        src = (
            "/**\n * @example\n * reverse([1, 2, 3]) // [3, 2, 1]\n */\n"
            "export function reverse<T>(xs: ReadonlyArray<T>): Array<T> { return [...xs].reverse(); }\n"
        )
        rec = next(r for r in t.records_from_source(src, None, v2=True, with_properties=True)
                   if r["schema_version"] == "0.2.0")
        self.assertEqual([p["name"] for p in rec.get("properties", [])], ["length_preserving"])
        self.assertEqual(rec["intent_tags"], ["lossless"])


class TestExecutableBodies(unittest.TestCase):
    """The body builder produces runnable bodies for `function`-declaration and arrow BLOCK
    single-`return` shapes (not just bare arrow expressions), normalizes strict equality, and stays
    byte-identical to the Python adapter (shared `nl_body` core)."""

    def _ts(self, name, src):
        from nl_body import body_ast_from_ts
        return body_ast_from_ts(name, src)

    def test_function_declaration_and_arrow_block_agree(self):
        # A `function` declaration and the equivalent arrow expression build the SAME body.
        decl = self._ts("inc", "export function inc(x: number): number { return x + 1; }")
        arrow = self._ts("inc", "(x: number) => x + 1")
        self.assertEqual(decl, arrow)
        self.assertEqual(decl["body"]["fn"], {"kind": "var", "name": "add"})

    def test_block_with_let_binding(self):
        b = self._ts("f", "(x: number) => { const y = x + 1; return y * 2; }")
        self.assertEqual(b["body"]["kind"], "let")
        self.assertEqual(b["body"]["name"], "y")

    def test_strict_equality_normalizes(self):
        self.assertEqual(self._ts("z", "(x: number) => x === 0")["body"]["fn"], {"kind": "var", "name": "eq"})
        self.assertEqual(self._ts("z", "(a: number, b: number) => a !== b")["body"]["fn"],
                         {"kind": "var", "name": "neq"})

    def test_logical_operators_normalize(self):
        # TS `&&`/`||` are Python `and`/`or` -> the `and`/`or` builtins.
        self.assertEqual(
            self._ts("both", "export function both(a: boolean, b: boolean): boolean { return a && b; }")
            ["body"]["fn"], {"kind": "var", "name": "and"})
        self.assertEqual(self._ts("either", "(a: boolean, b: boolean) => a || b")["body"]["fn"],
                         {"kind": "var", "name": "or"})

    def test_out_of_subset_returns_none(self):
        self.assertIsNone(self._ts("loop", "(x: number) => { let y = 0; for (;;) {} return y; }"))
        self.assertIsNone(self._ts("tern", "(x: number) => x > 0 ? 1 : 0"))  # TS ternary isn't Python

    def test_agrees_byte_for_byte_with_python_adapter(self):
        from nl_body import body_ast_from_ts, body_ast_from_py
        import ast as pyast
        for ts_src, py_src in [
            ("export function inc(x: number): number { return x + 1; }", "def inc(x):\n    return x + 1"),
            ("(x: number) => x === 0", "def z(x):\n    return x == 0"),
        ]:
            ts_addr = c.expr_address(body_ast_from_ts("f", ts_src))
            py_addr = c.expr_address(body_ast_from_py(pyast.parse(py_src).body[0]))
            self.assertEqual(ts_addr, py_addr, "TS and Python bodies must content-address alike")


if __name__ == "__main__":
    unittest.main(verbosity=2)
