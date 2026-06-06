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
from unittest import mock

from django.conf import settings
from django.core.management import call_command
from django.db import connection
from django.test import Client, TestCase, override_settings

from . import embedding
from .embedding import LexicalHashingEmbedder, NeuralEmbedder, cosine, get_embedder
from .models import Record
from .vectorindex import get_vector_index, store_vector

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
        v = self.emb.embed(raw)
        r = Record.objects.create(
            hash=h, kind="function-record", schema_version="0.1.0", raw=raw,
            name_hints=raw.get("name_hints", []), intent_tags=raw.get("intent_tags", []),
            effects=(raw.get("signature") or {}).get("effects", []),
            embedding=v, embedding_model=self.emb.model_id)
        store_vector(h, v)   # populate the pgvector column so search works on both backends
        return r

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


# --- neural embedder + vector index backends ------------------------------------------------------

class _FakeResp:
    """Stand-in for an http response context manager (so neural tests need no model server)."""
    def __init__(self, payload):
        self._b = json.dumps(payload).encode()
    def read(self):
        return self._b
    def __enter__(self):
        return self
    def __exit__(self, *a):
        return False


class NeuralEmbedderTests(TestCase):
    def test_embed_batch_parses_list_of_vectors(self):
        e = NeuralEmbedder("m", "http://x", dim=2)
        with mock.patch("commons.embedding.urllib.request.urlopen",
                        return_value=_FakeResp([[0.1, 0.2], [0.3, 0.4]])):
            self.assertEqual(e.embed_batch(["a", "b"]), [[0.1, 0.2], [0.3, 0.4]])

    def test_embed_single_uses_record_text(self):
        e = NeuralEmbedder("m", "http://x", dim=2)
        with mock.patch("commons.embedding.urllib.request.urlopen",
                        return_value=_FakeResp([[1.0, 0.0]])):
            self.assertEqual(e.embed(_MAP_REC), [1.0, 0.0])   # dict -> _record_text -> one input

    def test_tolerates_openai_response_shape(self):
        e = NeuralEmbedder("m", "http://x", dim=2)
        with mock.patch("commons.embedding.urllib.request.urlopen",
                        return_value=_FakeResp({"data": [{"embedding": [0.5, 0.6]}]})):
            self.assertEqual(e.embed("hi"), [0.5, 0.6])

    def test_requires_url(self):
        with self.assertRaises(ValueError):
            NeuralEmbedder("m", "", dim=2)


class EmbedderDispatchTests(TestCase):
    def setUp(self):
        embedding._CACHE.clear()

    def tearDown(self):
        embedding._CACHE.clear()

    @override_settings(COMMONS_EMBEDDER="lexical-hashing-v0", COMMONS_EMBEDDING_DIM=256)
    def test_lexical_is_default(self):
        self.assertIsInstance(get_embedder(), LexicalHashingEmbedder)

    @override_settings(COMMONS_EMBEDDER="bge-small-en-v1.5", COMMONS_EMBEDDING_DIM=384,
                       COMMONS_EMBEDDINGS_URL="http://x")
    def test_neural_id_selects_neural(self):
        e = get_embedder()
        self.assertIsInstance(e, NeuralEmbedder)
        self.assertEqual((e.model_id, e.dim), ("bge-small-en-v1.5", 384))

    @override_settings(COMMONS_EMBEDDER="bge-small-en-v1.5", COMMONS_EMBEDDINGS_URL="")
    def test_neural_without_url_raises(self):
        with self.assertRaises(ValueError):
            get_embedder()


class VectorIndexSelectionTests(TestCase):
    def test_index_matches_backend(self):
        from .vectorindex import PgVectorIndex, ScanIndex
        expected = PgVectorIndex if connection.vendor == "postgresql" else ScanIndex
        self.assertIsInstance(get_vector_index(), expected)


@unittest.skipUnless(connection.vendor == "postgresql", "pgvector ANN path requires Postgres")
class PgVectorIndexTests(TestCase):
    def test_ann_ranks_by_cosine(self):
        from .vectorindex import PgVectorIndex, get_vector_index, store_vector
        dim = settings.COMMONS_EMBEDDING_DIM

        def unit(i):
            v = [0.0] * dim
            v[i] = 1.0
            return v

        for h, i in [("fn_" + "a" * 64, 0), ("fn_" + "b" * 64, 1)]:
            Record.objects.create(hash=h, kind="function-record", schema_version="0.1.0",
                                  raw={}, embedding=unit(i), embedding_model="m")
            store_vector(h, unit(i))   # populate the pgvector column

        idx = get_vector_index()
        self.assertIsInstance(idx, PgVectorIndex)
        results, _ = idx.search(unit(0), 2, {}, "m")
        self.assertEqual(results[0]["hash"], "fn_" + "a" * 64)
        self.assertAlmostEqual(results[0]["score"], 1.0, places=5)


# --- .nlb seed bundles ----------------------------------------------------------------------------

_R1 = {"hash": "fn_" + "a" * 64, "schema_version": "0.1.0", "name_hints": ["a"]}
_R2 = {"hash": "fn_" + "b" * 64, "schema_version": "0.2.0", "name_hints": ["b"]}


def _frankenbundle(manifest_dict, record_dicts):
    """Build a raw .nlb with an arbitrary manifest + records (for tampering tests)."""
    import gzip
    import tarfile

    from .bundle import MANIFEST_NAME, RECORDS_NAME
    mb = (json.dumps(manifest_dict) + "\n").encode()
    rb = ("\n".join(json.dumps(r) for r in record_dicts) + ("\n" if record_dicts else "")).encode()
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as t:
        for name, data in [(MANIFEST_NAME, mb), (RECORDS_NAME, rb)]:
            ti = tarfile.TarInfo(name)
            ti.size = len(data)
            t.addfile(ti, io.BytesIO(data))
    return gzip.compress(buf.getvalue())


class BundleFormatTests(TestCase):
    def test_round_trip(self):
        from .bundle import read_bundle, write_bundle
        buf = io.BytesIO()
        write_bundle(buf, [_R1, _R2])
        manifest, records = read_bundle(io.BytesIO(buf.getvalue()))
        self.assertEqual(manifest["count"], 2)
        self.assertEqual(sorted(r["hash"] for r in records), sorted([_R1["hash"], _R2["hash"]]))
        self.assertEqual({r["hash"]: r for r in records}[_R1["hash"]], _R1)

    def test_deterministic_and_order_independent(self):
        from .bundle import write_bundle
        b1, b2 = io.BytesIO(), io.BytesIO()
        write_bundle(b1, [_R2, _R1])     # reversed input
        write_bundle(b2, [_R1, _R2])
        self.assertEqual(b1.getvalue(), b2.getvalue())

    def test_empty_bundle(self):
        from .bundle import read_bundle, write_bundle
        buf = io.BytesIO()
        write_bundle(buf, [])
        manifest, records = read_bundle(io.BytesIO(buf.getvalue()))
        self.assertEqual((manifest["count"], records), (0, []))

    def test_digest_mismatch_raises(self):
        from .bundle import BundleError, bundle_digest, read_bundle
        # manifest claims the digest of {R1,R2} but the payload holds only R1.
        bad = _frankenbundle(
            {"format_version": "nlb/1", "count": 2,
             "bundle_digest": bundle_digest([_R1["hash"], _R2["hash"]])},
            [_R1])
        with self.assertRaises(BundleError):
            read_bundle(io.BytesIO(bad))

    def test_not_a_bundle_raises(self):
        from .bundle import BundleError, read_bundle
        with self.assertRaises(BundleError):
            read_bundle(io.BytesIO(b"not a gzip"))


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class BundleCommandTests(TestCase):
    def test_export_then_load_round_trip(self):
        from .bundle import write_bundle  # noqa: F401 (ensures module imports under the gate)
        rec = _load("map.json")
        Client().post("/v0/records", data=json.dumps(rec), content_type="application/json")
        self.assertTrue(Record.objects.filter(hash=rec["hash"]).exists())

        with tempfile.NamedTemporaryFile(suffix=".nlb", delete=False) as f:
            path = f.name
        call_command("exportbundle", path)

        Record.objects.all().delete()                 # simulate a fresh node
        out = io.StringIO()
        call_command("loadbundle", path, stdout=out)
        self.assertIn("stored=1", out.getvalue())
        self.assertTrue(Record.objects.filter(hash=rec["hash"]).exists())

    def test_tampered_record_in_bundle_rejected(self):
        tampered = _load("map.json")
        tampered["name_hints"] = tampered.get("name_hints", []) + ["x"]   # hash no longer matches
        with tempfile.NamedTemporaryFile(suffix=".nlb", delete=False) as f:
            path = f.name
        from .bundle import write_bundle
        with open(path, "wb") as fh:
            write_bundle(fh, [tampered])              # packaging does not verify; ingest must

        out = io.StringIO()
        call_command("loadbundle", path, "--quiet", stdout=out)
        self.assertIn("stored=0", out.getvalue())
        self.assertIn("failed=1", out.getvalue())
        self.assertFalse(Record.objects.filter(hash=tampered["hash"]).exists())


class NlBundleConformanceTests(TestCase):
    def test_standalone_packager_is_byte_identical(self):
        # The standalone tooling/nl-bundle/nl_bundle.py must produce the SAME bytes as the node's
        # commons/bundle.py for the same records (the cross-implementation guarantee).
        import subprocess
        import sys

        from .bundle import write_bundle
        script = Path(settings.COMMONS_SPEC_DIR).parent / "tooling" / "nl-bundle" / "nl_bundle.py"
        self.assertTrue(script.exists(), script)

        records = [_R1, _R2]
        jsonl = ("\n".join(json.dumps(r) for r in records)).encode()
        proc = subprocess.run([sys.executable, str(script)], input=jsonl, capture_output=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        buf = io.BytesIO()
        write_bundle(buf, records)
        self.assertEqual(proc.stdout, buf.getvalue())
