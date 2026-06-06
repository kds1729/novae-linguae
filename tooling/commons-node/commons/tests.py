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

from .embedding import LexicalHashingEmbedder, cosine, get_embedder
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

    def test_publish_computes_embedding(self):
        # The ingest path (create_record) must populate the search vector + model id.
        rec = _load("map.json")
        self._publish(rec)
        row = Record.objects.get(hash=rec["hash"])
        self.assertEqual(row.embedding_model, get_embedder().model_id)
        self.assertTrue(row.embedding and len(row.embedding) == get_embedder().dim)

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


# --- semantic search ------------------------------------------------------------------------------
# These need no validator: they exercise the embedder + search ranking directly, creating Record rows
# with embeddings (the verification gate is tested above and is orthogonal to ranking).

_MAP_REC = {
    "name_hints": ["map", "fmap", "list_map"],
    "intent_tags": ["transform", "elementwise"],
    "signature": {"type": "forall a b. (a -> b) -> List a -> List b", "effects": [],
                  "complexity": "O(n)"},
    "properties": [{"name": "identity", "expr": "map(id, xs) == xs"}],
}
_B64_REC = {
    "name_hints": ["b64encode", "base64_encode"],
    "intent_tags": ["encoding"],
    "signature": {"type": "(bytes) -> bytes", "effects": []},
}


class EmbeddingTests(TestCase):
    def test_deterministic_across_instances(self):
        # Determinism is load-bearing (principle 5): two fresh embedders agree byte-for-byte.
        a = LexicalHashingEmbedder().embed(_MAP_REC)
        b = LexicalHashingEmbedder().embed(_MAP_REC)
        self.assertEqual(a, b)

    def test_vector_is_l2_normalized(self):
        v = get_embedder().embed(_MAP_REC)
        self.assertEqual(len(v), get_embedder().dim)
        self.assertAlmostEqual(sum(x * x for x in v) ** 0.5, 1.0, places=6)

    def test_empty_record_is_zero_vector(self):
        self.assertEqual(get_embedder().embed({}), [0.0] * get_embedder().dim)

    def test_relevant_query_is_closer(self):
        emb = get_embedder()
        q = emb.embed("map a function over each element of a list")
        self.assertGreater(cosine(q, emb.embed(_MAP_REC)), cosine(q, emb.embed(_B64_REC)))


class SearchTests(TestCase):
    def setUp(self):
        self.client = Client()
        self.emb = get_embedder()
        self.map_hash = "fn_" + "a" * 64
        self.b64_hash = "fn_" + "b" * 64
        self._mk(self.map_hash, _MAP_REC)
        self._mk(self.b64_hash, _B64_REC)

    def _mk(self, h, raw):
        return Record.objects.create(
            hash=h, kind="function-record", schema_version="0.1.0", raw=raw,
            name_hints=raw.get("name_hints", []), intent_tags=raw.get("intent_tags", []),
            effects=(raw.get("signature") or {}).get("effects", []),
            embedding=self.emb.embed(raw), embedding_model=self.emb.model_id)

    def _search(self, body):
        return self.client.post("/v0/search", data=json.dumps(body),
                                content_type="application/json")

    def test_query_ranks_relevant_first(self):
        resp = self._search({"query": "map a function over each element of a list", "k": 2})
        self.assertEqual(resp.status_code, 200)
        body = resp.json()
        self.assertEqual(body["model"], self.emb.model_id)
        self.assertEqual(body["results"][0]["hash"], self.map_hash)
        self.assertGreaterEqual(body["results"][0]["score"], body["results"][-1]["score"])

    def test_like_returns_target_near_one(self):
        resp = self._search({"like": self.map_hash, "k": 2})
        self.assertEqual(resp.status_code, 200)
        top = resp.json()["results"][0]
        self.assertEqual(top["hash"], self.map_hash)
        self.assertAlmostEqual(top["score"], 1.0, places=5)

    def test_filter_composes_with_search(self):
        # An intent filter that only the map record satisfies must exclude the base64 record.
        resp = self._search({"query": "encode", "filter": {"intent_tags": {"all": ["transform"]}}})
        hashes = [r["hash"] for r in resp.json()["results"]]
        self.assertIn(self.map_hash, hashes)
        self.assertNotIn(self.b64_hash, hashes)

    def test_k_caps_results(self):
        resp = self._search({"query": "list", "k": 1})
        self.assertEqual(len(resp.json()["results"]), 1)

    def test_missing_query_and_like_is_400(self):
        self.assertEqual(self._search({}).status_code, 400)

    def test_like_unknown_hash_is_404(self):
        self.assertEqual(self._search({"like": "fn_" + "c" * 64}).status_code, 404)

    def test_info_reports_embedding_model(self):
        self.assertEqual(self.client.get("/v0/info").json()["embedding_model"], self.emb.model_id)

    def test_embedrecords_backfills_null(self):
        bare = Record.objects.create(hash="fn_" + "d" * 64, kind="function-record",
                                     schema_version="0.1.0", raw=_MAP_REC)
        self.assertIsNone(bare.embedding)
        call_command("embedrecords", stdout=io.StringIO())
        bare.refresh_from_db()
        self.assertEqual(bare.embedding_model, self.emb.model_id)
        self.assertTrue(bare.embedding)
