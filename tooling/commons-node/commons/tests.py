"""Protocol tests for the commons node (spec/commons.md).

Exercises publish / resolve / exists / query / sync / info against the real example records, using
Django's in-process test client (no server, no Postgres). Verification shells out to the built
nl-validator; the whole suite skips if that binary is absent.

    python3 manage.py test
"""

import base64
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from django.conf import settings
from django.core.management import call_command
from django.db import connection
from django.test import Client, TestCase, override_settings

from . import embedding, query
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

    def test_publish_bare_body_is_self_addressing(self):
        # A body expression carries NO embedded hash — the whole expression IS the hashed content —
        # so the node computes its expr_… address on ingest and serves it back byte-exactly. This is
        # what lets a remote agent loop (`orchestrate --node`) resolve a record's body_hash.
        body = _load("body-double-second-field.json")
        resp = self._publish(body)
        self.assertEqual(resp.status_code, 201, resp.content)
        address = resp.json()["hash"]
        self.assertTrue(address.startswith("expr_"), address)
        # The address is the record's declared body_hash — the two halves link up.
        rec = _load("double-second-field.v0.2.json")
        self.assertEqual(address, rec["body_hash"])
        # Idempotent, and resolvable to the exact bare body.
        again = self._publish(body)
        self.assertEqual(again.status_code, 200)
        got = self.client.get(f"/v0/records/{address}")
        self.assertEqual(got.json(), body)

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

    # --- certifications (signed certification records + serve-by-subject) ----------------------

    def _make_certification(self, record="reverse.json", body="body-reverse.json",
                            seed="novae-linguae-example-certifier"):
        """Produce a signed certification for an example record via `nl-validator certify --sign`."""
        out = subprocess.run(
            [str(VALIDATOR), "certify", str(EXAMPLES / record), "--body", str(EXAMPLES / body),
             "--sign", seed],
            capture_output=True, text=True,
        )
        return json.loads(out.stdout)

    def test_certification_publishes_and_resolves(self):
        # A signed certification is a first-class artifact: it verifies on ingest (cert_ hash + Ed25519)
        # and resolves back byte-for-byte, exactly like a record or a message.
        cert = self._make_certification()
        self.assertTrue(cert["hash"].startswith("cert_"))
        resp = self._publish(cert)
        self.assertEqual(resp.status_code, 201, resp.content)
        got = self.client.get(f"/v0/records/{cert['hash']}")
        self.assertEqual(got.status_code, 200)
        self.assertEqual(got.json(), cert)

    def test_certifications_served_by_subject(self):
        # The trust-delegation face: certifications about a function are fetchable by its `fn_…` address.
        cert = self._make_certification()
        self._publish(cert)
        subject = cert["subject"]
        resp = self.client.get(f"/v0/records/{subject}/certifications")
        self.assertEqual(resp.status_code, 200)
        body = resp.json()
        self.assertEqual(body["subject"], subject)
        self.assertEqual(body["count"], 1)
        self.assertEqual(body["certifications"][0]["hash"], cert["hash"])
        # `?certified=true` returns only positive certifications (this one is certified).
        only = self.client.get(f"/v0/records/{subject}/certifications?certified=true").json()
        self.assertEqual(only["count"], 1)

    def test_certifications_absent_subject_is_empty(self):
        resp = self.client.get("/v0/records/fn_" + "0" * 64 + "/certifications")
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(resp.json()["count"], 0)

    def test_tampered_certification_rejected(self):
        # Flip the signed verdict — the Ed25519 signature no longer verifies, so ingest refuses it (422).
        cert = self._make_certification()
        cert["certified"] = not cert["certified"]
        resp = self._publish(cert)
        self.assertEqual(resp.status_code, 422)
        self.assertIn(resp.json()["error"], {"signature_invalid", "hash_mismatch"})

    # --- weights records + eval attestations (spec/weights.md) ---------------------------------

    def test_weights_record_publishes_and_resolves(self):
        # A weights POINTER record is a first-class artifact: schema-gated + wgt_ hash-verified on
        # ingest, resolved back byte-for-byte. The blobs it points at never enter the record store.
        rec = _load("weights-coder7b-c12-s1.json")
        self.assertTrue(rec["hash"].startswith("wgt_"))
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 201, resp.content)
        got = self.client.get(f"/v0/records/{rec['hash']}")
        self.assertEqual(got.status_code, 200)
        self.assertEqual(got.json(), rec)

    def test_tampered_weights_record_rejected(self):
        # Repointing the blob manifest breaks the wgt_ address — the gate refuses it.
        rec = _load("weights-coder7b-c12-s1.json")
        rec["files"][0]["sha256"] = "0" * 64
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 422)
        self.assertEqual(resp.json()["error"], "hash_mismatch")

    def test_eval_attestations_served_by_subject(self):
        # The weights counterpart of certifications-by-subject: a consumer that resolved a wgt_
        # pointer fetches the signed eval attestations about it, then judges under its OWN policy.
        att = _load("attest-coder7b-c12-s1.json")
        self.assertTrue(att["hash"].startswith("evl_"))
        resp = self._publish(att)
        self.assertEqual(resp.status_code, 201, resp.content)
        subject = att["subject"]
        got = self.client.get(f"/v0/records/{subject}/attestations")
        self.assertEqual(got.status_code, 200)
        body = got.json()
        self.assertEqual(body["subject"], subject)
        self.assertEqual(body["count"], 1)
        self.assertEqual(body["attestations"][0]["hash"], att["hash"])

    def test_tampered_eval_attestation_rejected(self):
        # Inflating the signed score fails Ed25519 verification on ingest — accountability holds.
        att = _load("attest-coder7b-c12-s1.json")
        att["results"]["write"]["pass"] = att["results"]["write"]["total"]
        resp = self._publish(att)
        self.assertEqual(resp.status_code, 422)
        self.assertIn(resp.json()["error"], {"signature_invalid", "hash_mismatch"})

    def test_blob_store_and_serve(self):
        # /v0/blobs/{sha256}: content-addressed, gate-free, verified client-side. `addblob` stores.
        with tempfile.TemporaryDirectory() as tmp:
            blob = Path(tmp) / "adapter_model.safetensors"
            blob.write_bytes(b"weights bytes, opaque to the commons")
            import hashlib
            digest = hashlib.sha256(blob.read_bytes()).hexdigest()
            with override_settings(COMMONS_BLOB_DIR=str(Path(tmp) / "blobs")):
                out = io.StringIO()
                call_command("addblob", str(blob), stdout=out)
                self.assertIn(digest, out.getvalue())
                self.assertIn("stored", out.getvalue())
                # Idempotent.
                out2 = io.StringIO()
                call_command("addblob", str(blob), stdout=out2)
                self.assertIn("already present", out2.getvalue())
                # HEAD then GET; the served bytes hash back to the address.
                self.assertEqual(self.client.head(f"/v0/blobs/{digest}").status_code, 200)
                got = self.client.get(f"/v0/blobs/{digest}")
                self.assertEqual(got.status_code, 200)
                served = b"".join(got.streaming_content)
                self.assertEqual(hashlib.sha256(served).hexdigest(), digest)
        # Absent blob is a 404.
        self.assertEqual(self.client.get("/v0/blobs/" + "f" * 64).status_code, 404)

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

    def test_query_include_summary(self):
        # The discovery-cost projection: decision fields present, heavy record contents absent.
        rec = _load("map.json")
        self._publish(rec)
        resp = self.client.post("/v0/query?include=summary",
                                data=json.dumps({"name_hint_prefix": "map"}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        summ = next(s for s in resp.json()["results"] if s["hash"] == rec["hash"])
        # signature-judging fields are there...
        self.assertIn("List", summ["type"])
        self.assertIn("elementwise", summ["intent_tags"])
        self.assertEqual(summ["kind"], "function-record")
        # ...but not the full record (body/examples/properties/signature object)
        for heavy in ("examples", "properties", "signature", "refinements", "raw"):
            self.assertNotIn(heavy, summ)

    def test_query_rank_by_intent_fit(self):
        # Both records satisfy the filter; the one carrying MORE of the requested intent tags ranks first.
        one, two = "fn_" + "1" * 64, "fn_" + "2" * 64
        Record.objects.create(hash=one, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform"])
        Record.objects.create(hash=two, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform", "elementwise"])
        resp = self.client.post("/v0/query?rank=relevance",
                                data=json.dumps({"intent_tags": {"any": ["transform", "elementwise"]}}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        results = resp.json()["results"]
        self.assertEqual(results[0], two)                       # two matched tags -> ranked first
        self.assertLess(results.index(two), results.index(one))

    def test_query_rank_certified_boost(self):
        # Equal intent fit; the certified record is surfaced first (verified-quality boost).
        plain, cert = "fn_" + "3" * 64, "fn_" + "4" * 64
        Record.objects.create(hash=plain, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform"])
        Record.objects.create(hash=cert, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform"], certified=True)
        resp = self.client.post("/v0/query?rank=relevance",
                                data=json.dumps({"intent_tags": {"any": ["transform"]}}),
                                content_type="application/json")
        results = resp.json()["results"]
        self.assertEqual(results[0], cert)
        self.assertLess(results.index(cert), results.index(plain))

    def test_query_default_order_is_unranked(self):
        # Without ?rank, order stays insertion (id) — ranking is strictly opt-in, pagination unaffected.
        first, second = "fn_" + "5" * 64, "fn_" + "6" * 64
        Record.objects.create(hash=first, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform"])
        Record.objects.create(hash=second, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform", "elementwise"])
        resp = self.client.post("/v0/query",
                                data=json.dumps({"intent_tags": {"any": ["transform", "elementwise"]}}),
                                content_type="application/json")
        # second would outrank first if ranked, but default order is by insertion id
        self.assertEqual(resp.json()["results"], [first, second])

    def _make_summary_records(self, n):
        # n records that all match `{"intent_tags": {"any": ["transform"]}}`, each with a non-trivial
        # type string so its summary has real token weight (the budget cap operates on summary size).
        for i in range(n):
            Record.objects.create(
                hash="fn_" + str(i) * 64, kind="function-record", schema_version="0.2.0", raw={},
                intent_tags=["transform"], name_hints=[f"widget_{i}"],
                type_str="forall a b. (a -> b) -> List a -> List b")

    def test_query_token_budget_trims_summary(self):
        # A token budget smaller than the full page returns fewer summaries than matched, with a report.
        self._make_summary_records(6)
        full = self.client.post("/v0/query?include=summary",
                                data=json.dumps({"intent_tags": {"any": ["transform"]}}),
                                content_type="application/json").json()
        self.assertEqual(len(full["results"]), 6)
        self.assertNotIn("budget", full)                       # no budget field when none requested
        per = query.summary_tokens(full["results"][0])         # cost of one summary
        resp = self.client.post("/v0/query?include=summary",
                                data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                 "token_budget": per * 2 + 1}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        body = resp.json()
        self.assertEqual(len(body["results"]), 2)              # only two summaries fit
        self.assertEqual(body["budget"]["returned"], 2)
        self.assertTrue(body["budget"]["more"])                # more matched than fit
        self.assertLessEqual(body["budget"]["tokens_estimated"], per * 2 + 1)
        self.assertFalse(body["complete"])                     # trimmed => not complete
        # cursor continues past the last INCLUDED record, so the next page picks up where budget cut off
        nxt = self.client.post("/v0/query?include=summary",
                               data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                "cursor": body["cursor"]}),
                               content_type="application/json").json()
        returned = {s["hash"] for s in body["results"]} | {s["hash"] for s in nxt["results"]}
        self.assertEqual(len(returned), 6)                     # no overlap, no gap

    def test_query_token_budget_always_returns_top(self):
        # A budget too small for even one summary still returns the top result (never an empty page).
        self._make_summary_records(3)
        resp = self.client.post("/v0/query?include=summary",
                                data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                 "token_budget": 1}),
                                content_type="application/json")
        body = resp.json()
        self.assertEqual(len(body["results"]), 1)
        self.assertGreater(body["budget"]["tokens_estimated"], 1)   # reports the overrun honestly
        self.assertTrue(body["budget"]["more"])

    def test_query_token_budget_generous_returns_all(self):
        self._make_summary_records(4)
        resp = self.client.post("/v0/query?include=summary",
                                data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                 "token_budget": 100000}),
                                content_type="application/json")
        body = resp.json()
        self.assertEqual(len(body["results"]), 4)
        self.assertFalse(body["budget"]["more"])
        self.assertTrue(body["complete"])

    def test_query_token_budget_respects_rank(self):
        # Budget + rank: the trimmed page is the top-ranked ones, not the id-first ones.
        self._make_summary_records(3)
        cert = "fn_" + "c" * 64
        Record.objects.create(hash=cert, kind="function-record", schema_version="0.2.0", raw={},
                              intent_tags=["transform"], name_hints=["widget_c"],
                              type_str="forall a b. (a -> b) -> List a -> List b", certified=True)
        per = query.summary_tokens(query.record_summary(Record.objects.get(hash=cert)))
        resp = self.client.post("/v0/query?include=summary&rank=relevance",
                                data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                 "token_budget": per + 1}),
                                content_type="application/json")
        body = resp.json()
        self.assertEqual(len(body["results"]), 1)
        self.assertEqual(body["results"][0]["hash"], cert)     # certified boost -> ranked first -> kept
        self.assertIsNone(body["cursor"])                      # ranking is not a paged feed

    def test_query_token_budget_ignored_without_summary(self):
        # A budget on the hashes-only tier is inert (uniform-size hashes aren't the context cost).
        self._make_summary_records(5)
        resp = self.client.post("/v0/query",
                                data=json.dumps({"intent_tags": {"any": ["transform"]},
                                                 "token_budget": 1}),
                                content_type="application/json")
        body = resp.json()
        self.assertEqual(len(body["results"]), 5)              # not trimmed
        self.assertNotIn("budget", body)

    def test_query_token_budget_invalid_is_400(self):
        for bad in (0, -5, "lots", 3.5, True):
            resp = self.client.post("/v0/query?include=summary",
                                    data=json.dumps({"token_budget": bad}),
                                    content_type="application/json")
            self.assertEqual(resp.status_code, 400, f"{bad!r}: {resp.content}")
            self.assertEqual(resp.json()["error"], "malformed_filter")

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

    def test_search_include_summary(self):
        # `?include=summary` folds the decision-field projection into each ranked hit, score preserved.
        resp = self.client.post("/v0/search?include=summary",
                                data=json.dumps({"query": "map a function over each element", "k": 2}),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        top = next(r for r in resp.json()["results"] if r["hash"] == self.map_hash)
        self.assertIn("score", top)                       # similarity kept
        self.assertIn("transform", top["intent_tags"])    # projection folded in
        self.assertNotIn("signature", top)                # not the full record

    def test_malformed_filter_is_400(self):
        # The typed filter follows the same contract as /v0/query: a bare list is a clean 400.
        resp = self._search({"query": "encode", "filter": {"effects": ["io.console"]}})
        self.assertEqual(resp.status_code, 400, resp.content)
        self.assertEqual(resp.json()["error"], "malformed_filter")

    def test_search_token_budget_trims_ranked_hits(self):
        # A budget fitting only the top hit trims the ranked+projected summaries to the best one.
        full = self.client.post("/v0/search?include=summary",
                                 data=json.dumps({"query": "map a function over each element", "k": 2}),
                                 content_type="application/json").json()
        self.assertEqual(len(full["results"]), 2)
        self.assertNotIn("budget", full)
        top_cost = query.summary_tokens(full["results"][0])
        resp = self.client.post(
            "/v0/search?include=summary",
            data=json.dumps({"query": "map a function over each element", "k": 2,
                             "token_budget": top_cost}),
            content_type="application/json")
        self.assertEqual(resp.status_code, 200)
        body = resp.json()
        self.assertEqual(len(body["results"]), 1)
        self.assertEqual(body["results"][0]["hash"], self.map_hash)   # highest similarity kept
        self.assertTrue(body["budget"]["more"])
        self.assertLessEqual(body["budget"]["tokens_estimated"], top_cost)
        self.assertNotIn("cursor", body)                              # search is ranked, not paged

    def test_search_token_budget_ignored_without_summary(self):
        resp = self._search({"query": "map a function over each element", "k": 2, "token_budget": 1})
        body = resp.json()
        self.assertEqual(len(body["results"]), 2)     # hashes+scores only; budget inert
        self.assertNotIn("budget", body)

    def test_search_token_budget_invalid_is_400(self):
        resp = self.client.post("/v0/search?include=summary",
                                data=json.dumps({"query": "map", "token_budget": 0}),
                                content_type="application/json")
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

    def test_manifest_conforms_to_bundle_schema(self):
        # Keeps spec/bundle.schema.json in sync with what bundle.py actually emits, without a
        # jsonschema dependency: check required fields, additionalProperties=false, the string
        # patterns, and the signature->producer dependency for both an unsigned and a signed manifest.
        import re
        from .bundle import build_manifest, write_bundle
        schema = json.loads((Path(settings.COMMONS_SPEC_DIR) / "bundle.schema.json").read_text())
        req, props = schema["required"], schema["properties"]

        def conforms(m):
            self.assertFalse(set(req) - set(m), f"missing required: {set(req) - set(m)}")
            self.assertFalse(set(m) - set(props), f"unknown keys: {set(m) - set(props)}")  # additionalProperties:false
            for key in ("bundle_digest", "producer", "signature"):
                if key in m:
                    self.assertRegex(m[key], props[key]["pattern"], key)
            self.assertEqual(m["format_version"], "nlb/1")
            self.assertIsInstance(m["count"], int)
            self.assertIsInstance(m["schema_versions"], list)
            for dep, needs in schema.get("dependentRequired", {}).items():
                if dep in m:
                    self.assertTrue(set(needs) <= set(m), f"{dep} requires {needs}")

        conforms(build_manifest([_R1, _R2], source={"repo": "https://github.com/org/lib",
                                                     "release": "v1.2.3"}))
        conforms(write_bundle(io.BytesIO(), [_R1, _R2], sign_seed=self.SEED))


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


# --- /v0/prove (optional proof service) -----------------------------------------------------------

import shutil as _shutil  # noqa: E402

_HAS_SOLVER = _shutil.which("z3") is not None or _shutil.which("cvc5") is not None


def _forall(vars, body):
    return {"kind": "forall", "vars": vars, "body": body}


def _ap(op, *args):
    return {"kind": "app", "op": op, "args": list(args)}


def _v(n):
    return {"kind": "var", "name": n}


def _int(n):
    return {"kind": "lit", "value": {"kind": "int", "value": n}}


# forall n. add(n, n) == mul(2, n) — first-order, PROVED by SMT (no body, no induction).
DOUBLING_LAW = {
    "schema_version": "0.2.0",
    "properties": [{"name": "doubling", "expr": _forall(
        ["n"], _ap("eq", _ap("add", _v("n"), _v("n")), _ap("mul", _int(2), _v("n"))))}],
}


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class ProveEndpointTests(TestCase):
    def setUp(self):
        self.client = Client()

    def _prove(self, payload):
        return self.client.post("/v0/prove", data=json.dumps(payload), content_type="application/json")

    def test_inline_first_order_law(self):
        r = self._prove({"record": DOUBLING_LAW})
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertEqual(len(body["results"]), 1)
        status = body["results"][0]["status"]
        if _HAS_SOLVER:
            self.assertEqual(status, "PROVED", msg=body)
            self.assertEqual(body["summary"].get("proved"), 1)
        else:
            self.assertEqual(status, "NO-SOLVER")

    @unittest.skipUnless(_HAS_SOLVER, "no SMT solver on PATH")
    def test_refuted_law(self):
        # forall n. add(n, n) == n — false except at n = 0.
        false_law = {"schema_version": "0.2.0", "properties": [{"name": "bad", "expr": _forall(
            ["n"], _ap("eq", _ap("add", _v("n"), _v("n")), _v("n")))}]}
        r = self._prove({"record": false_law})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.json()["results"][0]["status"], "REFUTED")

    def test_record_without_properties_is_422(self):
        r = self._prove({"record": {"schema_version": "0.2.0"}})
        self.assertEqual(r.status_code, 422)
        self.assertEqual(r.json()["error"], "no_properties")

    def test_missing_target_is_400(self):
        self.assertEqual(self._prove({}).status_code, 400)

    def test_absent_hash_is_404(self):
        r = self._prove({"hash": "fn_" + "0" * 64})
        self.assertEqual(r.status_code, 404)

    def test_get_not_allowed(self):
        self.assertEqual(self.client.get("/v0/prove").status_code, 405)

    def test_info_advertises_prove(self):
        info = self.client.get("/v0/info").json()
        self.assertIn("prove", info)
        self.assertEqual(info["prove"]["available"], _HAS_SOLVER)


# --- /v0/equiv (semantic-equivalence service) -----------------------------------------------------

def _lam(param, body):
    return {"kind": "lambda", "params": [{"name": param}], "body": body}
def _bap(fn, *args):
    return {"kind": "app", "fn": {"kind": "var", "name": fn}, "args": list(args)}
def _bv(n):
    return {"kind": "var", "name": n}
def _bi(n):
    return {"kind": "lit", "value": {"kind": "int", "value": n}}

DOUBLE_ADD = _lam("n", _bap("add", _bv("n"), _bv("n")))        # \n -> add(n, n)
DOUBLE_MUL = _lam("m", _bap("mul", _bi(2), _bv("m")))          # \m -> mul(2, m)
SUCC = _lam("k", _bap("add", _bv("k"), _bi(1)))               # \k -> add(k, 1)


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class EquivEndpointTests(TestCase):
    def setUp(self):
        self.client = Client()

    def _equiv(self, f, g):
        return self.client.post("/v0/equiv", data=json.dumps({"f": f, "g": g}), content_type="application/json")

    def test_equivalent(self):
        r = self._equiv(DOUBLE_ADD, DOUBLE_MUL)
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.json()["verdict"], "equivalent" if _HAS_SOLVER else "no_solver")

    @unittest.skipUnless(_HAS_SOLVER, "no SMT solver on PATH")
    def test_distinct(self):
        r = self._equiv(DOUBLE_ADD, SUCC)
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.json()["verdict"], "distinct")

    def test_missing_operand_is_400(self):
        r = self.client.post("/v0/equiv", data=json.dumps({"f": DOUBLE_ADD}), content_type="application/json")
        self.assertEqual(r.status_code, 400)

    def test_get_not_allowed(self):
        self.assertEqual(self.client.get("/v0/equiv").status_code, 405)
