"""Protocol tests for the commons node (spec/commons.md).

Exercises publish / resolve / exists / query / sync / info against the real example records, using
Django's in-process test client (no server, no Postgres). Verification shells out to the built
nl-validator; the whole suite skips if that binary is absent.

    python3 manage.py test
"""

import io
import json
import tempfile
import unittest
from pathlib import Path

from django.conf import settings
from django.core.management import call_command
from django.test import Client, TestCase

from .models import Record

EXAMPLES = Path(settings.COMMONS_SPEC_DIR) / "examples"
VALIDATOR = Path(settings.COMMONS_VALIDATOR)


def _load(name):
    return json.loads((EXAMPLES / name).read_text())


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class CommonsProtocolTests(TestCase):
    def setUp(self):
        self.client = Client()

    def _publish(self, record):
        return self.client.post("/v0/records", data=json.dumps(record),
                                content_type="application/json")

    # --- publish / resolve / exists -----------------------------------------------------------

    def test_publish_is_idempotent(self):
        rec = _load("map.json")
        first = self._publish(rec)
        self.assertEqual(first.status_code, 201)
        self.assertEqual(first.json(), {"hash": rec["hash"], "stored": True})

        again = self._publish(rec)
        self.assertEqual(again.status_code, 200)
        self.assertEqual(again.json(), {"hash": rec["hash"], "stored": False})

    def test_resolve_returns_exact_record(self):
        rec = _load("map.json")
        self._publish(rec)
        got = self.client.get(f"/v0/records/{rec['hash']}")
        self.assertEqual(got.status_code, 200)
        self.assertEqual(got.json(), rec)

    def test_head_exists(self):
        rec = _load("map.json")
        self.assertEqual(self.client.head(f"/v0/records/{rec['hash']}").status_code, 404)
        self._publish(rec)
        self.assertEqual(self.client.head(f"/v0/records/{rec['hash']}").status_code, 200)

    def test_absent_is_404(self):
        got = self.client.get("/v0/records/fn_" + "0" * 64)
        self.assertEqual(got.status_code, 404)
        self.assertEqual(got.json()["error"], "absent")

    def test_message_publishes_and_verifies(self):
        # request.json is a signed message; verify-on-ingest must check hash AND signature.
        msg = _load("request.json")
        resp = self._publish(msg)
        self.assertEqual(resp.status_code, 201, resp.content)

    # --- verification gate --------------------------------------------------------------------

    def test_tampered_record_rejected(self):
        rec = _load("map.json")
        rec["name_hints"] = rec.get("name_hints", []) + ["tampered"]  # hash no longer matches
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 422)
        self.assertEqual(resp.json()["error"], "hash_mismatch")
        self.assertFalse(self.client.head(f"/v0/records/{rec['hash']}").status_code == 200)

    def test_malformed_json_rejected(self):
        resp = self.client.post("/v0/records", data="{not json", content_type="application/json")
        self.assertEqual(resp.status_code, 400)
        self.assertEqual(resp.json()["error"], "malformed_json")

    def test_unknown_prefix_rejected(self):
        resp = self._publish({"hash": "zz_" + "0" * 64, "schema_version": "0.1.0"})
        self.assertEqual(resp.status_code, 422)
        self.assertEqual(resp.json()["error"], "unsupported_kind")

    # --- typed discovery ----------------------------------------------------------------------

    def test_query_by_intent_tag(self):
        rec = _load("map.json")
        self._publish(rec)
        resp = self.client.post("/v0/query", data=json.dumps({"intent_tags": {"any": ["elementwise"]}}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        self.assertIn(rec["hash"], resp.json()["results"])

    def test_query_effects_none_matches_pure(self):
        rec = _load("map.json")  # pure: effects == []
        self._publish(rec)
        resp = self.client.post("/v0/query", data=json.dumps({"effects": {"none": True}}),
                                content_type="application/json")
        self.assertIn(rec["hash"], resp.json()["results"])

    def test_query_name_hint_prefix(self):
        rec = _load("map.json")  # name_hints include "map"
        self._publish(rec)
        resp = self.client.post("/v0/query", data=json.dumps({"name_hint_prefix": "map"}),
                                content_type="application/json")
        self.assertIn(rec["hash"], resp.json()["results"])

    def test_query_non_matching_filter_excludes(self):
        rec = _load("map.json")
        self._publish(rec)
        resp = self.client.post("/v0/query", data=json.dumps({"intent_tags": {"all": ["nonexistent"]}}),
                                content_type="application/json")
        self.assertNotIn(rec["hash"], resp.json()["results"])

    def test_query_include_record(self):
        rec = _load("map.json")
        self._publish(rec)
        resp = self.client.post("/v0/query?include=record",
                                data=json.dumps({"name_hint_prefix": "map"}),
                                content_type="application/json")
        self.assertIn(rec, resp.json()["records"])

    # --- federation feed / metadata -----------------------------------------------------------

    def test_sync_feed(self):
        rec = _load("map.json")
        self._publish(rec)
        resp = self.client.get("/v0/sync?since=0")
        self.assertEqual(resp.status_code, 200)
        self.assertIn(rec["hash"], resp.json()["hashes"])
        # cursor advances past the published row
        self.assertGreater(resp.json()["cursor"], 0)

    def test_info(self):
        self._publish(_load("map.json"))
        body = self.client.get("/v0/info").json()
        self.assertEqual(body["protocol"], "v0")
        self.assertEqual(body["record_count"], 1)
        self.assertIn("function-record", body["kinds"])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class LoadRecordsCommandTests(TestCase):
    def test_loadrecords_jsonl(self):
        # The adapter→commons pipeline: a JSONL stream of records (good + tampered).
        good = _load("map.json")
        tampered = _load("map.json")
        tampered["name_hints"] = tampered.get("name_hints", []) + ["x"]  # hash no longer matches
        lines = [json.dumps(good), json.dumps(tampered), json.dumps(good)]  # last is a dup
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write("\n".join(lines) + "\n")
            path = f.name

        out = io.StringIO()
        call_command("loadrecords", path, "--quiet", stdout=out)
        self.assertEqual(out.getvalue().strip(), "stored=1 skipped=1 failed=1")
        self.assertEqual(Record.objects.count(), 1)
        self.assertTrue(Record.objects.filter(hash=good["hash"]).exists())
