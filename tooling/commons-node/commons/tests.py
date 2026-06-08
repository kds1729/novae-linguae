"""Protocol tests for the commons node (spec/commons.md).

Exercises publish / resolve / exists / query / sync / info against the real example records, using
Django's in-process test client (no server, no Postgres). Verification shells out to the built
nl-validator; the whole suite skips if that binary is absent.

    python3 manage.py test
"""

import base64
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

    def test_query_malformed_array_predicate_is_400(self):
        # A bare list where an object predicate is required is a clean 400 (not an AttributeError 500).
        resp = self.client.post("/v0/query", data=json.dumps({"effects": ["io.console"]}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 400, resp.content)
        self.assertEqual(resp.json()["error"], "malformed_filter")

    def test_query_unknown_predicate_key_is_400(self):
        resp = self.client.post("/v0/query", data=json.dumps({"effects": {"bogus": ["x"]}}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 400, resp.content)

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

    def test_malformed_filter_is_400(self):
        # The typed filter follows the same contract as /v0/query: a bare list is a clean 400.
        resp = self._search({"query": "encode", "filter": {"effects": ["io.console"]}})
        self.assertEqual(resp.status_code, 400, resp.content)
        self.assertEqual(resp.json()["error"], "malformed_filter")

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


class BundleSigningTests(TestCase):
    SEED = "bundle-publisher-seed"

    def test_sign_then_verify_round_trip(self):
        from .bundle import read_bundle, verify_manifest, write_bundle
        buf = io.BytesIO()
        write_bundle(buf, [_R1, _R2], sign_seed=self.SEED)
        manifest, _ = read_bundle(io.BytesIO(buf.getvalue()))
        status, producer = verify_manifest(manifest)
        self.assertEqual(status, "valid")
        self.assertTrue(producer.startswith("did:nova:"))
        self.assertEqual(manifest["producer"], producer)

    def test_tampered_signed_manifest_is_invalid(self):
        from .bundle import verify_manifest, write_bundle
        m = write_bundle(io.BytesIO(), [_R1, _R2], sign_seed=self.SEED)   # returns signed manifest
        m["count"] = 999                                                  # alter a signed field
        self.assertEqual(verify_manifest(m)[0], "invalid")

    def test_unsigned_reports_unsigned(self):
        from .bundle import read_bundle, verify_manifest, write_bundle
        buf = io.BytesIO()
        write_bundle(buf, [_R1])
        manifest, _ = read_bundle(io.BytesIO(buf.getvalue()))
        self.assertEqual(verify_manifest(manifest)[0], "unsigned")

    def test_signed_bundle_is_deterministic(self):
        from .bundle import write_bundle
        a, b = io.BytesIO(), io.BytesIO()
        write_bundle(a, [_R1, _R2], sign_seed=self.SEED)
        write_bundle(b, [_R2, _R1], sign_seed=self.SEED)
        self.assertEqual(a.getvalue(), b.getvalue())


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

    def test_export_since_is_incremental(self):
        from .bundle import read_bundle
        first, second = _load("map.json"), _load("double.v0.2.json")
        for rec in (first, second):
            Client().post("/v0/records", data=json.dumps(rec), content_type="application/json")
        first_id = Record.objects.get(hash=first["hash"]).id

        with tempfile.NamedTemporaryFile(suffix=".nlb", delete=False) as f:
            path = f.name
        call_command("exportbundle", path, "--since", str(first_id))   # only records after `first`
        manifest, records = read_bundle(path)
        self.assertEqual([r["hash"] for r in records], [second["hash"]])

    def test_signed_export_load_reports_provenance(self):
        rec = _load("map.json")
        Client().post("/v0/records", data=json.dumps(rec), content_type="application/json")
        with tempfile.NamedTemporaryFile(suffix=".nlb", delete=False) as f:
            path = f.name
        call_command("exportbundle", path, "--sign-seed", "node-seed")
        Record.objects.all().delete()
        out = io.StringIO()
        call_command("loadbundle", path, "--require-signed", stdout=out)   # enforces a valid sig
        self.assertIn("provenance=signed by did:nova:", out.getvalue())
        self.assertIn("stored=1", out.getvalue())

    def test_require_signed_rejects_unsigned(self):
        from django.core.management.base import CommandError
        Client().post("/v0/records", data=json.dumps(_load("map.json")),
                      content_type="application/json")
        with tempfile.NamedTemporaryFile(suffix=".nlb", delete=False) as f:
            path = f.name
        call_command("exportbundle", path)                                 # unsigned
        with self.assertRaises(CommandError):
            call_command("loadbundle", path, "--require-signed", stdout=io.StringIO())


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

    def test_signed_packager_is_byte_identical(self):
        import subprocess
        import sys

        from .bundle import write_bundle
        script = Path(settings.COMMONS_SPEC_DIR).parent / "tooling" / "nl-bundle" / "nl_bundle.py"
        records = [_R1, _R2]
        jsonl = ("\n".join(json.dumps(r) for r in records)).encode()
        proc = subprocess.run([sys.executable, str(script), "--sign-seed", "shared-seed"],
                              input=jsonl, capture_output=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        buf = io.BytesIO()
        write_bundle(buf, records, sign_seed="shared-seed")
        self.assertEqual(proc.stdout, buf.getvalue())   # same nl_crypto -> identical signed bytes


# --- censorship-resistant bootstrap ---------------------------------------------------------------

class BootstrapTests(TestCase):
    SEED = "bootstrap-publisher"

    def _did(self):
        from .bundle import _crypto
        return _crypto().signing_keypair_from_user_seed(self.SEED)[2]

    def test_build_and_verify_trust_levels(self):
        from .bootstrap import build_descriptor, verify_descriptor
        doc = build_descriptor(["https://n1"], sign_seed=self.SEED)
        self.assertEqual(verify_descriptor(doc)[0], "valid")
        self.assertEqual(verify_descriptor(doc, trusted_dids=[doc["producer"]])[0], "valid")
        self.assertEqual(verify_descriptor(doc, trusted_dids=["did:nova:" + "0" * 64])[0], "untrusted")

    def test_resolve_falls_back_across_urls(self):
        from .bootstrap import build_descriptor, resolve
        good = json.dumps(build_descriptor(["https://n1"], sign_seed=self.SEED)).encode()

        def fetch(url):
            if url == "down":
                raise OSError("unreachable")
            return good

        doc, status, producer, src = resolve(["down", "ok"], trusted_dids=[self._did()], fetch=fetch)
        self.assertEqual((status, src), ("valid", "ok"))
        self.assertEqual(producer, self._did())

    def test_resolve_requires_trust_when_given(self):
        from .bootstrap import BootstrapError, build_descriptor, resolve
        good = json.dumps(build_descriptor(["https://n1"], sign_seed=self.SEED)).encode()
        with self.assertRaises(BootstrapError):   # signed, but not by a trusted did
            resolve(["ok"], trusted_dids=["did:nova:" + "0" * 64], fetch=lambda u: good)

    def test_resolve_unsigned_is_advisory(self):
        from .bootstrap import build_descriptor, resolve
        doc = build_descriptor(["https://n1"])    # unsigned
        _d, status, _p, _s = resolve(["x"], fetch=lambda u: json.dumps(doc).encode())
        self.assertEqual(status, "unsigned")

    def test_makebootstrap_writes_signed_descriptor(self):
        from .bootstrap import verify_descriptor
        with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as f:
            path = f.name
        call_command("makebootstrap", path, "--peer", "https://n1",
                     "--bundle-hash", "blake2b:" + "0" * 64, "--bundle-url", "https://m/commons.nlb",
                     "--sign-seed", self.SEED, stderr=io.StringIO())
        doc = json.loads(Path(path).read_text())
        self.assertEqual(doc["peers"], ["https://n1"])
        self.assertEqual(doc["latest_bundle"]["urls"], ["https://m/commons.nlb"])
        self.assertEqual(verify_descriptor(doc)[0], "valid")


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class BootstrapPullTests(TestCase):
    SEED = "bootstrap-publisher"

    def _did(self):
        from .bundle import _crypto
        return _crypto().signing_keypair_from_user_seed(self.SEED)[2]

    def test_pull_digest_mismatch_rejected(self):
        from .bootstrap import BootstrapError, build_descriptor, pull_bundle
        from .bundle import write_bundle
        bundle = io.BytesIO()
        write_bundle(bundle, [_load("map.json")])
        doc = build_descriptor([], latest_bundle={"hash": "blake2b:" + "0" * 64, "urls": ["mem"]})
        with self.assertRaises(BootstrapError):       # descriptor hash != bundle digest
            pull_bundle(doc, fetch=lambda u: bundle.getvalue())

    def test_bootstrap_command_pull_via_file_urls(self):
        # End-to-end with no network: descriptor + bundle on disk, fetched via file:// URLs.
        from .bundle import write_bundle
        rec = _load("map.json")
        tmp = Path(tempfile.mkdtemp())
        bundle_path = tmp / "commons.nlb"
        manifest = write_bundle(str(bundle_path), [rec])
        desc_path = tmp / "bootstrap.json"
        call_command("makebootstrap", str(desc_path), "--peer", "https://n1",
                     "--bundle-hash", manifest["bundle_digest"],
                     "--bundle-url", f"file://{bundle_path}", "--sign-seed", self.SEED,
                     stderr=io.StringIO())

        Record.objects.all().delete()                 # simulate a stranded, empty node
        out = io.StringIO()
        call_command("bootstrap", "--from", f"file://{desc_path}", "--trust", self._did(),
                     "--pull", stdout=out)
        self.assertIn("provenance=valid", out.getvalue())
        self.assertIn("stored=1", out.getvalue())
        self.assertTrue(Record.objects.filter(hash=rec["hash"]).exists())


class _FakeWS:
    """A scripted WebSocket socket for Nostr tests (server text frames, no network)."""
    def __init__(self, messages):
        self.out = b"".join(self._frame(m) for m in messages)
        self.sent = []

    @staticmethod
    def _frame(text):                              # unmasked server text frame
        p = text.encode("utf-8")
        h = bytearray([0x81])
        if len(p) < 126:
            h.append(len(p))
        elif len(p) < 65536:
            h.append(126)
            h += len(p).to_bytes(2, "big")
        else:
            h.append(127)
            h += len(p).to_bytes(8, "big")
        return bytes(h) + p

    def sendall(self, b):
        self.sent.append(b)

    def recv(self, n):
        chunk, self.out = self.out[:n], self.out[n:]
        return chunk

    def close(self):
        pass


class BootstrapChannelTests(TestCase):
    """Offline tests for the pluggable channels: scheme dispatch + response parsing (the live
    transports are stubbed)."""

    def test_unknown_scheme_raises(self):
        from . import bootstrap as B
        with self.assertRaises(B.BootstrapError):
            B._dispatch("carrier-pigeon://x")

    def test_ipns_builds_gateway_url(self):
        from . import bootstrap as B
        seen = {}

        def fake_get(url, timeout=30):
            seen["url"] = url
            return b"DESC"

        with mock.patch.object(B, "_http_get", fake_get):
            self.assertEqual(B._dispatch("ipns://k51abc?gateway=https://gw.example"), b"DESC")
        self.assertEqual(seen["url"], "https://gw.example/ipns/k51abc")

    def test_dns_doh_decodes_base64_descriptor(self):
        from . import bootstrap as B
        desc = {"v": "nlb-bootstrap/1", "peers": ["https://n"]}
        b64 = base64.b64encode(json.dumps(desc).encode()).decode()
        doh = json.dumps({"Answer": [{"type": 16, "data": '"' + b64 + '"'}]}).encode()
        with mock.patch.object(B, "_http_get", lambda url, timeout=30: doh):
            self.assertEqual(json.loads(B._dispatch("dns://commons.example.org"))["peers"], ["https://n"])

    def test_chain_follows_pointer(self):
        from . import bootstrap as B
        pages = {"https://explorer.example/anchor": b"https://host/desc.json",
                 "https://host/desc.json": b'{"v":"nlb-bootstrap/1","peers":[]}'}
        with mock.patch.object(B, "_http_get", lambda url, timeout=30: pages[url]):
            self.assertEqual(json.loads(B._dispatch("chain://https://explorer.example/anchor"))["v"],
                             "nlb-bootstrap/1")

    def test_chain_json_path_pointer(self):
        from . import bootstrap as B
        pages = {"https://x/api": json.dumps({"result": {"data": "https://host/d.json"}}).encode(),
                 "https://host/d.json": b'{"v":"nlb-bootstrap/1","peers":["p"]}'}
        with mock.patch.object(B, "_http_get", lambda url, timeout=30: pages[url]):
            doc = json.loads(B._dispatch("chain://https://x/api#result.data"))
        self.assertEqual(doc["peers"], ["p"])

    def test_redirect_depth_guard(self):
        from . import bootstrap as B
        with mock.patch.object(B, "_http_get", lambda url, timeout=30: b"chain://https://x/loop"):
            with self.assertRaises(B.BootstrapError):
                B._dispatch("chain://https://x/loop")

    def test_resolve_over_a_channel(self):
        from . import bootstrap as B
        desc = B.build_descriptor(["https://n1"])      # unsigned
        with mock.patch.object(B, "_http_get", lambda url, timeout=30: json.dumps(desc).encode()):
            doc, status, _producer, src = B.resolve(["ipns://k51"])
        self.assertEqual((status, src), ("unsigned", "ipns://k51"))

    def test_ws_frame_codec_round_trips(self):
        from . import bootstrap as B
        cap = _FakeWS([])
        B._ws_send_text(cap, "hello")
        frame = cap.sent[0]
        self.assertEqual(frame[0], 0x81)               # FIN + text
        self.assertEqual(frame[1] & 0x80, 0x80)        # client frames are masked
        n, mask, masked = frame[1] & 0x7F, frame[2:6], frame[6:6 + (frame[1] & 0x7F)]
        self.assertEqual(n, 5)
        self.assertEqual(bytes(b ^ mask[i % 4] for i, b in enumerate(masked)), b"hello")

    def test_nostr_returns_newest_event_content(self):
        from . import bootstrap as B
        fake = _FakeWS([json.dumps(["EVENT", "nlb", {"created_at": 1, "content": "OLD"}]),
                        json.dumps(["EVENT", "nlb", {"created_at": 2, "content": "NEW"}]),
                        json.dumps(["EOSE", "nlb"])])
        content = B._nostr_newest_content("relay.example", "abcd", 30078,
                                          connect=lambda *a, **k: fake)
        self.assertEqual(content, "NEW")
        self.assertTrue(fake.sent)                      # the REQ frame was sent

    # --- channel breadth: redundancy + new transports ---------------------------------------------

    def test_ipns_falls_back_across_gateways(self):
        from . import bootstrap as B
        tried = []

        def fake_get(url, timeout=30):
            tried.append(url)
            if "gw1" in url:
                raise OSError("gw1 down")
            return b"DESC"

        with mock.patch.object(B, "_http_get", fake_get):
            self.assertEqual(B._dispatch("ipns://k51?gateway=https://gw1,https://gw2"), b"DESC")
        self.assertEqual(tried, ["https://gw1/ipns/k51", "https://gw2/ipns/k51"])

    def test_dns_falls_back_across_resolvers(self):
        from . import bootstrap as B
        b64 = base64.b64encode(b'{"v":"nlb-bootstrap/1","peers":["p"]}').decode()
        ok = json.dumps({"Answer": [{"type": 16, "data": '"' + b64 + '"'}]}).encode()

        def fake_get(url, timeout=30):
            if "doh1" in url:
                raise OSError("doh1 blocked")
            return ok

        with mock.patch.object(B, "_http_get", fake_get):
            doc = json.loads(B._dispatch("dns://commons.example?doh=https://doh1/q,https://doh2/q"))
        self.assertEqual(doc["peers"], ["p"])

    def test_nostr_picks_newest_across_relays(self):
        from . import bootstrap as B
        events = {
            "relayA": {"created_at": 5, "content": "A5"},
            "relayB": {"created_at": 9, "content": "B9"},   # newest wins
        }
        with mock.patch.object(B, "_nostr_newest_event",
                               lambda relay, author, kind: events[relay]):
            self.assertEqual(B._dispatch("nostr://relayA,relayB/abcd"), b"B9")

    def test_nostr_one_dead_relay_does_not_sink_it(self):
        from . import bootstrap as B

        def newest(relay, author, kind):
            if relay == "down":
                raise B.BootstrapError("relay down")
            return {"created_at": 1, "content": "OK"}

        with mock.patch.object(B, "_nostr_newest_event", newest):
            self.assertEqual(B._dispatch("nostr://down,up/abcd"), b"OK")

    def test_mirror_tries_each_target_first_success_wins(self):
        from . import bootstrap as B
        pages = {"https://b/desc.json": b'{"v":"nlb-bootstrap/1","peers":["m"]}'}

        def fake_get(url, timeout=30):
            if url not in pages:
                raise OSError("not reachable")
            return pages[url]

        with mock.patch.object(B, "_http_get", fake_get):
            doc = json.loads(B._dispatch("mirror://https://a/desc.json|https://b/desc.json"))
        self.assertEqual(doc["peers"], ["m"])

    def test_onion_tunnels_over_socks5(self):
        from . import bootstrap as B
        seen = {}

        def fake_socks(proxy_host, proxy_port, host, port, path, timeout=30):
            seen.update(proxy_host=proxy_host, host=host, port=port, path=path)
            return b'{"v":"nlb-bootstrap/1","peers":["onion-peer"]}'

        with mock.patch.object(B, "_socks5_http_get", fake_socks):
            doc = json.loads(B._dispatch("onion://abcdef.onion/bootstrap.json"))
        self.assertEqual(doc["peers"], ["onion-peer"])
        self.assertEqual((seen["host"], seen["port"], seen["path"]),
                         ("abcdef.onion", 80, "/bootstrap.json"))

    def test_socks5_codec_speaks_the_handshake(self):
        from . import bootstrap as B
        # A fake socket scripting the SOCKS5 reply, then an HTTP response.
        recvs = [b"\x05\x00",                     # method selection: no-auth
                 b"\x05\x00\x00\x01",             # CONNECT reply, ATYP=IPv4
                 b"\x00\x00\x00\x00\x00\x00",     # bound addr (4) + port (2)
                 b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nHI", b""]
        sent = []

        class FakeSock:
            def sendall(self, b): sent.append(b)
            def recv(self, n): return recvs.pop(0) if recvs else b""
            def close(self): pass

        with mock.patch.object(B.socket, "create_connection", lambda *a, **k: FakeSock()):
            body = B._socks5_http_get("127.0.0.1", 9050, "svc.onion", 80, "/d")
        self.assertEqual(body, b"HI")
        self.assertEqual(sent[0], b"\x05\x01\x00")           # SOCKS5 greeting
        self.assertIn(b"\x05\x01\x00\x03", sent[1])          # CONNECT with domain ATYP
