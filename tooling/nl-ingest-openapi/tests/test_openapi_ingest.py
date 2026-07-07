"""Offline tests for the OpenAPI ingestion adapter — no network, no live service.

Generates records from the reference item-store description and checks:
  1. every generated record CERTIFIES (typecheck / effects / termination / complexity);
  2. the FAITHFULNESS contract — the generated bodyless-verb bodies are byte-identical to the
     hand-authored GW6 records (`item_status` / `delete_item`), so machine generation reproduces
     what a human wrote from the same description.

Run:  python3 -m unittest discover -s tooling/nl-ingest-openapi/tests
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_ADAPTER = _HERE.parent
REPO_ROOT = _ADAPTER.parent.parent
VALIDATOR = REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"
EXAMPLES = REPO_ROOT / "spec" / "examples"
SPEC = _ADAPTER / "examples" / "item-store.openapi.json"

sys.path.insert(0, str(_ADAPTER))
import openapi_ingest as oi  # noqa: E402


def _body_hash(record_path):
    return json.load(open(record_path))["body_hash"]


class OpenApiIngestTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="nl-openapi-")
        # main() writes records then sys.exit()s on the certify verdict — the records are on disk
        # regardless; the certify result itself is asserted by test_every_record_certifies.
        try:
            oi.main([str(SPEC), "--out", cls.tmp, "--secret-name", "api_token"])
        except SystemExit:
            pass
        cls.recs = {p.name.replace(".v0.2.json", ""): p
                    for p in Path(cls.tmp).glob("*.v0.2.json")}

    def test_all_operations_generated(self):
        self.assertEqual(set(self.recs), {"healthcheck", "putitem", "getitemstatus", "deleteitem"})

    @unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
    def test_every_record_certifies(self):
        for name, rp in self.recs.items():
            bp = Path(self.tmp) / f"body-{name}.json"
            r = subprocess.run([str(VALIDATOR), "certify", str(rp), "--body", str(bp),
                                "--records", self.tmp], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"{name} did not certify:\n{r.stdout}\n{r.stderr}")

    def test_faithful_to_hand_authored_gw6_records(self):
        # The bodyless verbs reproduce the hand-authored GW6 bodies byte-for-byte.
        self.assertEqual(_body_hash(self.recs["getitemstatus"]),
                         _body_hash(EXAMPLES / "item-status.v0.2.json"))
        self.assertEqual(_body_hash(self.recs["deleteitem"]),
                         _body_hash(EXAMPLES / "delete-item.v0.2.json"))

    def test_effect_follows_method(self):
        eff = lambda n: json.load(open(self.recs[n]))["signature"]["effects"]
        self.assertEqual(eff("getitemstatus"), ["net.read"])
        self.assertEqual(eff("healthcheck"), ["net.read"])
        self.assertEqual(eff("putitem"), ["net.write"])
        self.assertEqual(eff("deleteitem"), ["net.write"])

    def test_unauthenticated_op_has_no_secret(self):
        # /health declares `security: []` — no auth header, so no secret placeholder in its body.
        body = json.dumps(json.load(open(Path(self.tmp) / "body-healthcheck.json")))
        self.assertNotIn("secret", body)
        put = json.dumps(json.load(open(Path(self.tmp) / "body-putitem.json")))
        self.assertIn("{{secret:api_token}}", put)


if __name__ == "__main__":
    unittest.main()
