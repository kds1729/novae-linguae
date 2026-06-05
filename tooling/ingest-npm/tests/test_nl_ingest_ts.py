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


if __name__ == "__main__":
    unittest.main(verbosity=2)
