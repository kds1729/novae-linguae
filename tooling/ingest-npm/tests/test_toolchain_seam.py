"""Tests for the nl-ingest-ts toolchain seam (the injectable `enrich` source transform).

The real TypeScript-compiler backend (ts_enrich.js) needs node + typescript; here we verify the
SEAM mechanics with a stub enricher, so the test is independent of the toolchain being installed.
A gated end-to-end test runs ts_enrich.js only when `typescript` is resolvable.
"""

import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-npm
import nl_ingest_ts as ts  # noqa: E402


def _rettype(rec):
    """The rendered return type from a v0.1 record's type string `(...) -> RET`."""
    return rec["signature"]["type"].split("->")[-1].strip()


class SeamTests(unittest.TestCase):
    def test_enrich_none_is_scanner_default(self):
        # An arrow const with no return annotation -> scanner can't recover the return type.
        src = "export const f = (a: number) => a;"
        rec = ts.records_from_source(src)[0]
        self.assertEqual(_rettype(rec), "unknown")

    def test_enrich_feeds_resolved_signature_through(self):
        # A stub enricher mimicking `tsc` declaration emit: explicit, resolved return type.
        def stub(_source):
            return "export declare function f(a: number): number;"
        rec = ts.records_from_source("export const f = (a: number) => a;", enrich=stub)[0]
        self.assertEqual(_rettype(rec), "number")
        self.assertEqual(rec["name_hints"][0], "f")

    def test_enrich_failure_falls_back_to_original(self):
        # ts_enrich returns the source unchanged when node/typescript are missing — same as scanner.
        same = ts.ts_enrich("export function g(x: string): string { return x; }")
        self.assertIn("function g", same)


@unittest.skipUnless(
    shutil.which("node")
    and subprocess.run(["node", "-e", "require.resolve('typescript')"],
                       capture_output=True).returncode == 0,
    "node + typescript not available",
)
class GhciIntegration(unittest.TestCase):
    def test_real_tsc_resolves_inferred_return_type(self):
        src = "export function add(a: number, b: number) { return a + b; }"
        out = ts.ts_enrich(src)
        # The compiler-emitted declaration carries the inferred `: number` return type.
        self.assertIn("number", out)
        rec = ts.records_from_source(src, enrich=ts.ts_enrich)[0]
        self.assertEqual(_rettype(rec), "number")


if __name__ == "__main__":
    unittest.main()
