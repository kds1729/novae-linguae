"""Tests for the nl-ingest-hs toolchain seam (the injectable `enrich` source transform) and the
GHC -Wmissing-signatures parser. The real GHC backend needs `ghc`; the seam mechanics and the
diagnostic parser are tested here without it. A gated test runs ghc when present.
"""

import sys
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
sys.path.insert(0, str(_HERE.parents[1]))  # tooling/ingest-haskell
import nl_ingest_hs as hs  # noqa: E402
from nl_toolchain import tool_on_path  # noqa: E402


class GhcWarningParserTests(unittest.TestCase):
    def test_signature_on_same_line(self):
        text = "Demo.hs:3:1: warning: [-Wmissing-signatures]\n    Top-level binding with no type signature: inc :: Int -> Int\n"
        self.assertEqual(hs.parse_missing_signatures(text), ["inc :: Int -> Int"])

    def test_signature_on_next_line(self):
        text = (
            "Demo.hs:5:1: warning: [-Wmissing-signatures]\n"
            "    • Top-level binding with no type signature:\n"
            "        catMaybes :: [Maybe a] -> [a]\n"
        )
        self.assertEqual(hs.parse_missing_signatures(text), ["catMaybes :: [Maybe a] -> [a]"])

    def test_no_warnings(self):
        self.assertEqual(hs.parse_missing_signatures("All good, no diagnostics."), [])


class SeamTests(unittest.TestCase):
    def test_enrich_none_is_scanner_default(self):
        # No signature in source -> the scanner yields no record for `inc` (it keys off signatures).
        src = "module Demo (inc) where\ninc x = x + 1\n"
        names = {r["name_hints"][0] for r in hs.records_from_source(src, None, False)}
        self.assertNotIn("inc", names)

    def test_enrich_injects_signature_for_scanner(self):
        # A stub enricher mimicking GHC: prepend the inferred signature; the scanner now sees `inc`.
        def stub(source):
            return "inc :: Int -> Int\n" + source
        src = "module Demo (inc) where\ninc x = x + 1\n"
        recs = hs.records_from_source(src, None, False, enrich=stub)
        names = {r["name_hints"][0] for r in recs}
        self.assertIn("inc", names)

    def test_enrich_failure_falls_back(self):
        # With ghc absent, hs_enrich returns the source unchanged.
        if tool_on_path("ghc"):
            self.skipTest("ghc present; fallback path not exercised")
        src = "module Demo (inc) where\ninc x = x + 1\n"
        self.assertEqual(hs.hs_enrich(src), src)


@unittest.skipUnless(tool_on_path("ghc"), "ghc not available")
class GhcIntegration(unittest.TestCase):
    def test_real_ghc_recovers_signature(self):
        src = "module Demo (inc) where\ninc x = x + (1 :: Int)\n"
        recs = hs.records_from_source(src, None, False, enrich=hs.hs_enrich)
        names = {r["name_hints"][0] for r in recs}
        self.assertIn("inc", names)


if __name__ == "__main__":
    unittest.main()
