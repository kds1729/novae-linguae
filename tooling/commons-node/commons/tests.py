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

    def test_publish_trace_is_self_addressing(self):
        # A recorded effect trace (spec/trace.schema.json) is hashless and self-addressing like a
        # bare body: the node validates it, computes its trc_… address, and serves it back — which
        # is what lets a third party replay-verify an `observed` assert it fetched by msg_… address.
        trace = _load("trace-greet.v0.1.json")
        resp = self._publish(trace)
        self.assertEqual(resp.status_code, 201, resp.content)
        address = resp.json()["hash"]
        self.assertTrue(address.startswith("trc_"), address)
        # The address is exactly what the worked example's observed claim references.
        assert_msg = _load("assert-observed.v0.2.json")
        self.assertEqual(address, assert_msg["body"]["claim"]["trace"])
        # Idempotent, and resolvable to the exact trace.
        again = self._publish(trace)
        self.assertEqual(again.status_code, 200)
        got = self.client.get(f"/v0/records/{address}")
        self.assertEqual(got.json(), trace)
        # The observed assert itself goes through the ordinary signed-message gate.
        self.assertEqual(self._publish(assert_msg).status_code, 201)

    def test_publish_by_address_example_record(self):
        # A record whose example carries its expected value BY ADDRESS (`result_blob`, the v0.2
        # by-address form for values too large to inline) passes the verify-then-store gate on
        # schema + hash alone: the node does not fetch or judge blobs — like a weights record's
        # manifest, the sha256 in the record is the consumer's security boundary, not the store's.
        rec = _load("double-second-field.v0.2.json")
        ex = rec["examples"][0]
        del ex["result"]
        ex["result_blob"] = {"sha256": "ab" * 32, "bytes": 123}
        rec.pop("hash")
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
        rec["hash"] = subprocess.run([str(VALIDATOR), "hash", f.name],
                                     capture_output=True, text=True).stdout.strip()
        self.assertTrue(rec["hash"].startswith("fn_"))
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 201, resp.content)
        got = self.client.get(f"/v0/records/{rec['hash']}")
        self.assertEqual(got.json()["examples"][0]["result_blob"]["sha256"], "ab" * 32)

    @override_settings(COMMONS_MAX_RECORD_BYTES=512)
    def test_oversized_record_draws_413(self):
        # The record-store size cap — the boundary that makes by-address example values necessary
        # at all (a multi-MB inline observed document must not enter the metadata index).
        rec = _load("map.json")
        rec["_padding"] = "x" * 1024  # guaranteed past the overridden cap before any verification
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 413)
        self.assertEqual(resp.json(), {"error": "too_large"})

    def test_publish_bare_variant_body(self):
        # `variant`/`tuple` are legal bare-body top-level kinds (a 0-argument body like `\-> None`
        # tops out at them); the gate's body-kind list was missing both — same latent hole as the
        # Rust validator's (fixed there in 006dfa4).
        body = {"kind": "variant", "tag": "None"}
        resp = self._publish(body)
        self.assertEqual(resp.status_code, 201, resp.content)
        self.assertTrue(resp.json()["hash"].startswith("expr_"), resp.content)

    def test_replicate_retries_transient_fetch_failures(self):
        # The first full production mirror silently missed 12 records: a transient fetch failure
        # was swallowed like a legitimate verification skip and the durable cursor advanced past
        # it — never retried. Now a fetch failure stops the run WITHOUT committing the cursor
        # (retry next interval), while an unverifiable record still skips permanently (a bad peer
        # must not wedge the cursor).
        from django.core.cache import cache

        from . import tasks

        peer = "https://peer.example"
        cache.delete(f"replicate_cursor:{peer}")
        rec = _load("map.json")
        trace = _load("trace-greet.v0.1.json")
        trace_addr = "trc_360f45009b20e152bd1489105fd95234da350d11c2341f308ef24d147a0bbd08"
        flaky_calls = {"n": 0}

        def fake_get(url, timeout=30):
            if "/v0/sync" in url:
                return {"hashes": [rec["hash"], trace_addr], "cursor": 2, "complete": True}
            if rec["hash"] in url:
                flaky_calls["n"] += 1
                if flaky_calls["n"] == 1:
                    raise OSError("connection reset (transient)")
                return rec
            if trace_addr in url:
                return trace
            raise AssertionError(f"unexpected fetch {url}")

        with mock.patch.object(tasks, "_get_json", side_effect=fake_get):
            first = tasks.replicate_peer(peer)
            self.assertEqual(first["mirrored"], 1, first)          # the healthy record landed
            self.assertEqual(first["fetch_failures"], 1, first)
            self.assertEqual(int(cache.get(f"replicate_cursor:{peer}") or 0), 0,
                             "cursor must not advance past a page with fetch failures")
            second = tasks.replicate_peer(peer)
            self.assertEqual(second["mirrored"], 1, second)        # the flaky record retried in
            self.assertEqual(second["fetch_failures"], 0, second)
            self.assertEqual(second["cursor"], 2)
        self.assertTrue(Record.objects.filter(hash=rec["hash"]).exists())
        self.assertTrue(Record.objects.filter(hash=trace_addr).exists())
        cache.delete(f"replicate_cursor:{peer}")

    def test_replicate_blobs_mirrors_referenced_blobs_hash_verified(self):
        # The blob half of replication: a mirrored record whose example value is by address
        # (result_blob) must stay CHECKABLE on the replica, so the blobs its records reference
        # are pulled from the peer's /v0/blobs and sha256-verified. Lying bytes are refused (and
        # re-counted next run — self-healing, no cursor); honest bytes land under their address.
        import hashlib as _hashlib
        import shutil as _shutil

        from . import tasks

        blob_dir = Path(tempfile.mkdtemp(prefix="nl-blobrepl-"))
        good_bytes = json.dumps({"kind": "int", "value": 10}).encode()
        good_sha = _hashlib.sha256(good_bytes).hexdigest()

        # A gate-valid record referencing the blob by address, stored locally (the mirror step).
        rec = _load("double-second-field.v0.2.json")
        ex = rec["examples"][0]
        del ex["result"]
        ex["result_blob"] = {"sha256": good_sha, "bytes": len(good_bytes)}
        rec.pop("hash")
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
        rec["hash"] = subprocess.run([str(VALIDATOR), "hash", f.name],
                                     capture_output=True, text=True).stdout.strip()
        self.assertEqual(self._publish(rec).status_code, 201)

        lying = {"n": 0}

        def fake_fetch(url, dest_tmp, timeout=300):
            self.assertIn(f"/v0/blobs/{good_sha}", url)
            lying["n"] += 1
            data = b"not the blob" if lying["n"] == 1 else good_bytes
            Path(dest_tmp).write_bytes(data)
            return _hashlib.sha256(data).hexdigest()

        try:
            with override_settings(COMMONS_BLOB_DIR=str(blob_dir)):
                with mock.patch.object(tasks, "_fetch_blob", side_effect=fake_fetch):
                    first = tasks.replicate_blobs("https://peer.example")
                    self.assertEqual(first["failures"], 1, first)   # lying bytes refused
                    self.assertEqual(first["fetched"], 0, first)
                    self.assertFalse((blob_dir / good_sha).exists(),
                                     "mismatched bytes must never be stored under the address")
                    second = tasks.replicate_blobs("https://peer.example")
                    self.assertEqual(second["fetched"], 1, second)  # re-counted and retried
                self.assertEqual((blob_dir / good_sha).read_bytes(), good_bytes)
                # Idempotent: nothing missing, nothing fetched.
                third = tasks.replicate_blobs("https://peer.example")
                self.assertEqual((third["missing"], third["fetched"]), (0, 0), third)
        finally:
            _shutil.rmtree(blob_dir, ignore_errors=True)

    def test_blob_egress_is_metered(self):
        # The blob store is the LARGEST egress class, and FileResponse is a streaming response —
        # which the governor used to skip entirely, exempting exactly the payloads the budget
        # exists to bound. Blob bytes now count (by Content-Length), and an exhausted budget
        # 503s the next request like any other.
        import shutil as _shutil

        from django.core.cache import cache

        from . import egress

        blob_dir = Path(tempfile.mkdtemp(prefix="nl-egress-"))
        payload = b"x" * 4096
        src = blob_dir / "payload.bin"
        src.write_bytes(payload)
        cache.delete(egress._window_key())
        try:
            with override_settings(COMMONS_BLOB_DIR=str(blob_dir),
                                   COMMONS_EGRESS_BUDGET_BYTES=6000):
                out = io.StringIO()
                call_command("addblob", str(src), stdout=out)
                sha = out.getvalue().split()[0]
                before, _, _ = egress.usage()
                resp = self.client.get(f"/v0/blobs/{sha}")
                self.assertEqual(resp.status_code, 200)
                b"".join(resp.streaming_content)  # drain like a real client
                after, _, _ = egress.usage()
                self.assertGreaterEqual(after - before, len(payload),
                                        "blob bytes must count against the budget")
                # The budget is now exhausted (4096 >= 6000 - overheads is false; fetch again).
                resp = self.client.get(f"/v0/blobs/{sha}")
                if resp.status_code == 200:
                    b"".join(resp.streaming_content)
                    resp = self.client.get(f"/v0/blobs/{sha}")
                self.assertEqual(resp.status_code, 503, "an exhausted budget must throttle blobs too")
                self.assertEqual(resp.json()["error"], "egress_budget_exhausted")
        finally:
            cache.delete(egress._window_key())
            _shutil.rmtree(blob_dir, ignore_errors=True)

    def test_bundle_export_import_carries_referenced_blobs(self):
        # Disaster recovery end to end: exportbundle carries the blobs the exported records
        # reference; loadbundle restores them into a FRESH blob store — so the restored
        # by-address record is checkable, not merely resolvable.
        import hashlib as _hashlib
        import shutil as _shutil

        data = json.dumps({"kind": "int", "value": 10}).encode()
        sha = _hashlib.sha256(data).hexdigest()
        rec = _load("double-second-field.v0.2.json")
        ex = rec["examples"][0]
        del ex["result"]
        ex["result_blob"] = {"sha256": sha, "bytes": len(data)}
        rec.pop("hash")
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(rec, f)
        rec["hash"] = subprocess.run([str(VALIDATOR), "hash", f.name],
                                     capture_output=True, text=True).stdout.strip()
        self.assertEqual(self._publish(rec).status_code, 201)

        src_dir = Path(tempfile.mkdtemp(prefix="nl-bundleblob-src-"))
        dst_dir = Path(tempfile.mkdtemp(prefix="nl-bundleblob-dst-"))
        bundle_path = src_dir / "out.nlb"
        (src_dir / sha).write_bytes(data)
        try:
            with override_settings(COMMONS_BLOB_DIR=str(src_dir)):
                call_command("exportbundle", str(bundle_path),
                             stdout=io.StringIO(), stderr=io.StringIO())
            with override_settings(COMMONS_BLOB_DIR=str(dst_dir)):
                out = io.StringIO()
                call_command("loadbundle", str(bundle_path), "--quiet",
                             stdout=out, stderr=io.StringIO())
            self.assertIn("blobs_stored=1/1", out.getvalue())
            self.assertEqual((dst_dir / sha).read_bytes(), data,
                             "the restored blob store must hold the referenced value")
        finally:
            _shutil.rmtree(src_dir, ignore_errors=True)
            _shutil.rmtree(dst_dir, ignore_errors=True)

    def test_referenced_blobs_covers_examples_and_weights_manifests(self):
        from . import tasks

        weights = _load("weights-c21-14b-s1.json")
        shas = tasks._referenced_blobs(weights)
        self.assertEqual(shas, {f["sha256"] for f in weights["files"]})
        fn = _load("double-second-field.v0.2.json")
        self.assertEqual(tasks._referenced_blobs(fn), set(), "inline examples reference no blobs")

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

    def test_blob_carriage_round_trips_hash_verified(self):
        # A bundle may carry the blobs its records reference (blobs/<sha256> members), so a
        # restored by-address record is CHECKABLE on a node that never saw the origin. Members are
        # self-verifying by name on write AND read; a blobless bundle stays byte-identical to the
        # pre-blob-carriage format.
        import gzip
        import hashlib as _hashlib
        import tarfile

        from .bundle import BundleError, read_bundle_full, write_bundle
        data = b'{"kind":"int","value":10}'
        sha = _hashlib.sha256(data).hexdigest()
        buf = io.BytesIO()
        manifest = write_bundle(buf, [_R1], blobs={sha: data})
        self.assertEqual(manifest["blobs"], {"count": 1, "bytes": len(data)})
        m2, records, blobs = read_bundle_full(io.BytesIO(buf.getvalue()))
        self.assertEqual(blobs, {sha: data})
        self.assertEqual(len(records), 1)
        # Write-side refusal: content that hashes elsewhere never produces a bundle.
        with self.assertRaises(BundleError):
            write_bundle(io.BytesIO(), [_R1], blobs={sha: b"other bytes"})
        # Read-side refusal: a tampered member fails the whole read.
        raw = gzip.decompress(buf.getvalue())
        tampered = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(raw)) as src_tar:
            with tarfile.open(fileobj=tampered, mode="w") as dst_tar:
                for m in src_tar.getmembers():
                    content = src_tar.extractfile(m).read()
                    if m.name == f"blobs/{sha}":
                        content = content + b" "
                        m.size = len(content)
                    dst_tar.addfile(m, io.BytesIO(content))
        with self.assertRaises(BundleError):
            read_bundle_full(io.BytesIO(gzip.compress(tampered.getvalue())))
        # No blobs -> byte-identical to the legacy layout (determinism preserved).
        legacy, again = io.BytesIO(), io.BytesIO()
        write_bundle(legacy, [_R1])
        write_bundle(again, [_R1], blobs={})
        self.assertEqual(legacy.getvalue(), again.getvalue())


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


# --- structured type matching (`type_pattern`, commons.md open question 5) ------------------------

def _B(n):
    return {"kind": "builtin", "name": n}


def _FN(ps, r):
    return {"kind": "fn", "params": ps, "result": r}


def _AP(c, args):
    return {"kind": "apply", "ctor": _B(c), "args": args}


def _V(n):
    return {"kind": "var", "name": n}


_STR_TO_MAYBE_BOOL = _FN([_B("string")], _AP("Maybe", [_B("bool")]))
_TWO_STR_PREDICATE = _FN([_B("string"), _B("string")], _B("bool"))
_TWO_STR_BUILDER = _FN([_B("string"), _B("string")], _AP("Maybe", [_B("string")]))
_POLY_ID = {"kind": "forall", "vars": ["b"], "body": _FN([_V("b")], _V("b"))}
_NAT_SUCC = _FN([_B("nat")], _B("nat"))


class TypePatternMatchTests(TestCase):
    """Unit semantics of typematch.matches_type — the unifier behind the `type_pattern` filter."""

    def _m(self, pattern, ty):
        from .typematch import matches_type
        return matches_type(pattern, json.dumps(ty))

    def test_exact_structural_match(self):
        self.assertTrue(self._m(_STR_TO_MAYBE_BOOL, _STR_TO_MAYBE_BOOL))

    def test_builtin_names_are_exact(self):
        # `int` does not match a declared `nat` — a caller who accepts either says so with any_of.
        self.assertFalse(self._m(_FN([_B("int")], _B("int")), _NAT_SUCC))
        self.assertTrue(self._m(
            _FN([{"kind": "any_of", "types": [_B("int"), _B("nat")]}],
                {"kind": "any_of", "types": [_B("int"), _B("nat")]}),
            _NAT_SUCC))

    def test_any_wildcard(self):
        self.assertTrue(self._m(_FN([{"kind": "any"}], {"kind": "any"}), _NAT_SUCC))
        self.assertTrue(self._m(_FN([{"kind": "any"}], {"kind": "any"}), _STR_TO_MAYBE_BOOL))
        # arity still binds: a 2-param pattern does not match a 1-param function
        self.assertFalse(self._m(_FN([{"kind": "any"}, {"kind": "any"}], {"kind": "any"}), _NAT_SUCC))

    def test_head_matches_bare_and_applied_ctor(self):
        maybe_head = _FN([_B("string")], {"kind": "head", "names": ["Maybe"]})
        self.assertTrue(self._m(maybe_head, _STR_TO_MAYBE_BOOL))
        self.assertFalse(self._m(maybe_head, _FN([_B("string")], _B("bool"))))
        # bare builtin head
        self.assertTrue(self._m({"kind": "head", "names": ["Json", "Map"]}, _B("Json")))

    def test_pattern_var_consistency(self):
        # {a} -> {a} finds the polymorphic identity and a monomorphic endo, not int -> string.
        endo = _FN([_V("a")], _V("a"))
        self.assertTrue(self._m(endo, _POLY_ID))
        self.assertTrue(self._m(endo, _FN([_B("int")], _B("int"))))
        self.assertFalse(self._m(endo, _FN([_B("int")], _B("string"))))

    def test_record_var_consistency(self):
        # The record's `forall a. a -> a`: a pattern demanding different concrete ends must not match.
        self.assertFalse(self._m(_FN([_B("int")], _B("string")), _POLY_ID))
        self.assertTrue(self._m(_FN([_B("int")], _B("int")), _POLY_ID))

    def test_forall_stripped_both_sides(self):
        self.assertTrue(self._m({"kind": "forall", "vars": ["x"], "body": _FN([_V("x")], _V("x"))},
                                _POLY_ID))

    def test_v01_string_type_never_matches(self):
        from .typematch import matches_type
        self.assertFalse(matches_type({"kind": "any"}, "forall a. List a -> List a"))
        self.assertFalse(matches_type({"kind": "any"}, None))

    def test_splits_predicate_from_builder(self):
        # The GW16 unsplittable-fits shape: two (string, string) functions split by RESULT type.
        pred = _FN([_B("string"), _B("string")], _B("bool"))
        self.assertTrue(self._m(pred, _TWO_STR_PREDICATE))
        self.assertFalse(self._m(pred, _TWO_STR_BUILDER))

    def test_sum_record_tuple_structural(self):
        sum_t = {"kind": "sum", "variants": [{"tag": "None"}, {"tag": "Just", "type": _B("int")}]}
        self.assertTrue(self._m(sum_t, sum_t))
        self.assertFalse(self._m(sum_t, {"kind": "sum", "variants": [{"tag": "None"}]}))
        rec_t = {"kind": "record", "fields": [{"name": "status", "type": _B("int")}]}
        self.assertTrue(self._m(rec_t, rec_t))
        self.assertFalse(self._m(rec_t, {"kind": "record", "fields": [{"name": "code", "type": _B("int")}]}))
        tup = {"kind": "tuple", "elems": [_B("int"), _B("bool")]}
        self.assertTrue(self._m(tup, tup))
        self.assertFalse(self._m(tup, {"kind": "tuple", "elems": [_B("bool"), _B("int")]}))

    def test_validate_rejects_malformed(self):
        from .typematch import PatternError, validate_pattern
        for bad in (["fn"], {"kind": "nope"}, {"kind": "any_of", "types": []},
                    {"kind": "head", "names": []}, {"kind": "fn", "params": [], "result": None}):
            with self.assertRaises(PatternError):
                validate_pattern(bad)


class TypePatternQueryTests(TestCase):
    """`type_pattern` on POST /v0/query — server-side narrowing by structured type."""

    def setUp(self):
        self.client = Client()
        Record.objects.create(hash="fn_" + "a" * 64, kind="function-record", schema_version="0.2.0",
                              raw={}, terminates="always", intent_tags=["query"],
                              type_str=json.dumps(_TWO_STR_PREDICATE))
        Record.objects.create(hash="fn_" + "b" * 64, kind="function-record", schema_version="0.2.0",
                              raw={}, terminates="always", intent_tags=["query"],
                              type_str=json.dumps(_TWO_STR_BUILDER))
        Record.objects.create(hash="fn_" + "c" * 64, kind="function-record", schema_version="0.1.0",
                              raw={}, terminates="always", intent_tags=["query"],
                              type_str="(string, string) -> bool")  # v0.1 surface string

    def _q(self, flt):
        return self.client.post("/v0/query", data=json.dumps(flt), content_type="application/json")

    def test_pattern_narrows_to_result_sort(self):
        resp = self._q({"intent_tags": {"any": ["query"]},
                        "type_pattern": _FN([_B("string"), _B("string")], _B("bool"))})
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(resp.json()["results"], ["fn_" + "a" * 64])

    def test_pattern_with_head_result(self):
        resp = self._q({"type_pattern": _FN([_B("string"), _B("string")],
                                            {"kind": "head", "names": ["Maybe"]})})
        self.assertEqual(resp.json()["results"], ["fn_" + "b" * 64])

    def test_string_typed_record_excluded(self):
        # All three records carry the intent; only the two structured ones can match any pattern.
        resp = self._q({"intent_tags": {"any": ["query"]},
                        "type_pattern": _FN([{"kind": "any"}, {"kind": "any"}], {"kind": "any"})})
        self.assertEqual(set(resp.json()["results"]), {"fn_" + "a" * 64, "fn_" + "b" * 64})

    def test_malformed_pattern_is_400(self):
        resp = self._q({"type_pattern": {"kind": "nope"}})
        self.assertEqual(resp.status_code, 400)
        self.assertIn("kind", resp.json().get("detail", "") + resp.json().get("error", ""))


# --- type artifacts + matching through refs ---------------------------------------------------------

@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class TypeArtifactTests(TestCase):
    """`type_…` artifacts through the gate, and `type_pattern` matching THROUGH a `ref`."""

    def setUp(self):
        self.client = Client()

    @staticmethod
    def _addressed(expr):
        """The expr plus its real, validator-computed `type_…` address."""
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(expr, f)
        addr = subprocess.run([str(VALIDATOR), "hash", f.name, "--kind", "type"],
                              capture_output=True, text=True).stdout.strip()
        rec = dict(expr)
        rec["hash"] = addr
        return rec

    def _publish(self, record):
        return self.client.post("/v0/records", data=json.dumps(record),
                                content_type="application/json")

    def test_type_record_through_the_gate(self):
        rec = self._addressed(_STR_TO_MAYBE_BOOL)
        self.assertTrue(rec["hash"].startswith("type_"), rec["hash"])
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 201, resp.content)
        got = self.client.get(f"/v0/records/{rec['hash']}")
        self.assertEqual(got.json(), rec)
        # Idempotent; tampered content under the same address refuses.
        self.assertEqual(self._publish(rec).status_code, 200)
        bad = dict(rec)
        bad["result"] = _B("int")
        self.assertEqual(self._publish(bad).status_code, 422)

    def test_illformed_type_refused(self):
        # Schema-valid but ill-formed: a free type variable (no enclosing forall) fails check-type.
        rec = self._addressed(_FN([_V("a")], _V("b")))
        resp = self._publish(rec)
        self.assertEqual(resp.status_code, 422, resp.content)

    def test_pattern_matches_through_a_ref(self):
        # A published type definition; a function whose declared type REFERENCES it nominally.
        defn = self._addressed(_AP("Maybe", [_B("bool")]))
        self.assertEqual(self._publish(defn).status_code, 201)
        nominal = _FN([_B("string")], {"kind": "ref", "target": defn["hash"]})
        Record.objects.create(hash="fn_" + "d" * 64, kind="function-record",
                              schema_version="0.2.0", raw={}, terminates="always",
                              type_str=json.dumps(nominal))
        # A STRUCTURAL pattern finds the nominally-typed record (resolution at match time)...
        resp = self.client.post("/v0/query", content_type="application/json",
                                data=json.dumps({"type_pattern":
                                                 _FN([_B("string")], _AP("Maybe", [_B("bool")]))}))
        self.assertIn("fn_" + "d" * 64, resp.json()["results"])
        # ...and so does a pattern that names the ref itself, and a head pattern through the ref.
        for result_pattern in ({"kind": "ref", "target": defn["hash"]},
                               {"kind": "head", "names": ["Maybe"]}):
            resp = self.client.post("/v0/query", content_type="application/json",
                                    data=json.dumps({"type_pattern": _FN([_B("string")], result_pattern)}))
            self.assertIn("fn_" + "d" * 64, resp.json()["results"], result_pattern)
        # A structurally-different pattern still refuses.
        resp = self.client.post("/v0/query", content_type="application/json",
                                data=json.dumps({"type_pattern": _FN([_B("string")], _B("bool"))}))
        self.assertNotIn("fn_" + "d" * 64, resp.json()["results"])

    def test_unresolvable_and_cyclic_refs_do_not_match_structurally(self):
        from .typematch import matches_type

        absent = {"kind": "ref", "target": "type_" + "9" * 64}
        ty = json.dumps(_FN([_B("string")], absent))
        # Unresolvable: matches `any`, not a structural pattern.
        self.assertTrue(matches_type(_FN([_B("string")], {"kind": "any"}), ty, load_type=lambda t: None))
        self.assertFalse(matches_type(_FN([_B("string")], _B("bool")), ty, load_type=lambda t: None))
        # A cyclic alias chain terminates and does not match structurally.
        a, b = "type_" + "a" * 64, "type_" + "b" * 64
        aliases = {a: {"kind": "ref", "target": b}, b: {"kind": "ref", "target": a}}
        cyc = json.dumps(_FN([_B("string")], {"kind": "ref", "target": a}))
        self.assertFalse(matches_type(_FN([_B("string")], _B("bool")), cyc,
                                      load_type=aliases.get))


# --- equivalence claims (`equivalent`, spec/claim-expression) --------------------------------------

@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class EquivalenceClaimTests(TestCase):
    """The `equivalent` claim kind through the gate + GET /v0/records/{fn}/equivalences."""

    FN_A = "fn_" + "1" * 64
    FN_B = "fn_" + "2" * 64
    FN_C = "fn_" + "3" * 64

    def setUp(self):
        self.client = Client()

    def _signed_equiv_assert(self, a, b, seed="equivalence-asserter", domain=None):
        claim = {"kind": "equivalent", "a": a, "b": b, "method": "normal-form"}
        if domain is not None:
            claim["domain"] = domain
        envelope = {
            "schema_version": "0.2.0", "kind": "assert", "to": None, "in_reply_to": None,
            "timestamp": "2026-07-13T00:00:00Z", "constraints": None,
            "body": {"subject": a, "claim": claim, "evidence": None},
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(envelope, f)
        out = subprocess.run([str(VALIDATOR), "sign", f.name, "--seed", seed],
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        return json.loads(out.stdout)

    def test_signed_equivalence_assert_passes_the_gate(self):
        # The message gate needed no code change: the claim schema gained the kind, and a signed
        # assert carrying it verifies like any other message.
        msg = self._signed_equiv_assert(self.FN_A, self.FN_B)
        resp = self.client.post("/v0/records", data=json.dumps(msg),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 201, resp.content)
        got = self.client.get(f"/v0/records/{msg['hash']}")
        self.assertEqual(got.json()["body"]["claim"]["kind"], "equivalent")

    def test_equivalences_endpoint_filters_by_either_endpoint(self):
        one = self._signed_equiv_assert(self.FN_A, self.FN_B)
        other = self._signed_equiv_assert(self.FN_B, self.FN_C, seed="second-asserter")
        for m in (one, other):
            self.assertEqual(self.client.post("/v0/records", data=json.dumps(m),
                                              content_type="application/json").status_code, 201)
        # Something non-equivalent that must not leak in.
        rec = _load("map.json")
        self.client.post("/v0/records", data=json.dumps(rec), content_type="application/json")

        about_a = self.client.get(f"/v0/records/{self.FN_A}/equivalences").json()
        self.assertEqual(about_a["count"], 1)
        self.assertEqual(about_a["equivalences"][0]["hash"], one["hash"])
        about_b = self.client.get(f"/v0/records/{self.FN_B}/equivalences").json()
        self.assertEqual(about_b["count"], 2, "b appears in both claims (either endpoint matches)")
        none = self.client.get(f"/v0/records/{rec['hash']}/equivalences").json()
        self.assertEqual(none["count"], 0)

    def test_equivalences_get_only(self):
        resp = self.client.post(f"/v0/records/{self.FN_A}/equivalences")
        self.assertEqual(resp.status_code, 405)

    def test_query_collapse_equivalent(self):
        # Three same-intent functions, two of them claimed equivalent (a signed, gate-verified
        # assert): the collapse view returns one representative per class and reports the merge;
        # without the flag all three come back — the view is strictly opt-in.
        for fill in ("1", "2", "3"):
            Record.objects.create(hash="fn_" + fill * 64, kind="function-record",
                                  schema_version="0.2.0", raw={}, terminates="always",
                                  intent_tags=["collapse-demo"])
        msg = self._signed_equiv_assert(self.FN_A, self.FN_B)
        self.assertEqual(self.client.post("/v0/records", data=json.dumps(msg),
                                          content_type="application/json").status_code, 201)
        flt = json.dumps({"intent_tags": {"any": ["collapse-demo"]}})
        plain = self.client.post("/v0/query", data=flt, content_type="application/json").json()
        self.assertEqual(len(plain["results"]), 3)
        self.assertNotIn("collapsed", plain)
        merged = self.client.post("/v0/query?collapse=equivalent", data=flt,
                                  content_type="application/json").json()
        self.assertEqual(merged["results"], [self.FN_A, self.FN_C])
        self.assertEqual(merged["collapsed"], {self.FN_A: [self.FN_B]})

    def test_domain_qualified_claim_passes_the_gate_but_never_collapses(self):
        # A DOMAIN-QUALIFIED equivalence (`∀x. domain(x) ⇒ a(x) = b(x)`, spec/claim-expression)
        # licenses substitution only ON the domain: the gate admits it, /equivalences serves it,
        # and the collapse view must NEVER merge on it — substituting one address for another in
        # arbitrary applications is exactly what the qualifier withholds.
        domain = {"vars": ["n"], "expr": {"kind": "app", "op": "ge", "args": [
            {"kind": "var", "name": "n"}, {"kind": "lit", "value": {"kind": "int", "value": 0}}]}}
        for fill in ("1", "2"):
            Record.objects.create(hash="fn_" + fill * 64, kind="function-record",
                                  schema_version="0.2.0", raw={}, terminates="always",
                                  intent_tags=["domain-demo"])
        msg = self._signed_equiv_assert(self.FN_A, self.FN_B, domain=domain)
        resp = self.client.post("/v0/records", data=json.dumps(msg),
                                content_type="application/json")
        self.assertEqual(resp.status_code, 201, resp.content)

        served = self.client.get(f"/v0/records/{self.FN_A}/equivalences").json()
        self.assertEqual(served["count"], 1, "the claim stays queryable")
        self.assertEqual(served["equivalences"][0]["body"]["claim"]["domain"], domain)

        flt = json.dumps({"intent_tags": {"any": ["domain-demo"]}})
        merged = self.client.post("/v0/query?collapse=equivalent", data=flt,
                                  content_type="application/json").json()
        self.assertEqual(sorted(merged["results"]), [self.FN_A, self.FN_B],
                         "both candidates survive — no merge on a domain-qualified claim")
        self.assertNotIn("collapsed", merged)


# --- body storage tiering (commons.md open question 4) ---------------------------------------------

@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class BodyTieringTests(TestCase):
    """A bare body past the record cap is admitted, tiered into the blob store, and resolves
    byte-equivalently; everything that is not a body keeps the record cap."""

    def setUp(self):
        self.client = Client()
        self.blob_dir = tempfile.mkdtemp(prefix="nl-tier-blobs-")

    @staticmethod
    def _big_body(n=5000):
        # A valid bare body (a string literal) whose canonical JSON exceeds the (overridden) cap.
        return {"kind": "lit", "value": {"kind": "string", "value": "x" * n}}

    def _publish(self, record):
        return self.client.post("/v0/records", data=json.dumps(record),
                                content_type="application/json")

    def test_oversized_body_tiers_and_resolves(self):
        with override_settings(COMMONS_MAX_RECORD_BYTES=2048, COMMONS_BLOB_DIR=self.blob_dir):
            body = self._big_body()
            resp = self._publish(body)
            self.assertEqual(resp.status_code, 201, resp.content)
            addr = resp.json()["hash"]
            self.assertTrue(addr.startswith("expr_"))

            row = Record.objects.get(hash=addr)
            self.assertEqual(row.raw, {}, "the metadata index holds only a pointer row")
            self.assertTrue(row.blob_sha256)
            blob_path = Path(self.blob_dir) / row.blob_sha256
            self.assertTrue(blob_path.is_file())
            self.assertEqual(row.blob_bytes, blob_path.stat().st_size)

            # Resolve is indistinguishable from an inline record.
            got = self.client.get(f"/v0/records/{addr}")
            self.assertEqual(got.status_code, 200)
            self.assertEqual(json.loads(b"".join(got.streaming_content)), body)
            self.assertEqual(self.client.head(f"/v0/records/{addr}").status_code, 200)

            # Idempotent republish.
            again = self._publish(body)
            self.assertEqual(again.status_code, 200)
            self.assertFalse(again.json()["stored"])

    def test_oversized_non_body_still_413(self):
        with override_settings(COMMONS_MAX_RECORD_BYTES=2048, COMMONS_BLOB_DIR=self.blob_dir):
            rec = _load("map.json")
            rec["_padding"] = "x" * 4096
            resp = self._publish(rec)
            self.assertEqual(resp.status_code, 413)

    def test_body_ceiling_still_bounds(self):
        with override_settings(COMMONS_MAX_RECORD_BYTES=1024, COMMONS_MAX_BODY_BYTES=2048,
                               COMMONS_BLOB_DIR=self.blob_dir):
            resp = self._publish(self._big_body(4096))
            self.assertEqual(resp.status_code, 413)

    def test_under_cap_body_stays_inline(self):
        with override_settings(COMMONS_MAX_RECORD_BYTES=1 << 20, COMMONS_BLOB_DIR=self.blob_dir):
            body = _load("body-double-second-field.json")
            resp = self._publish(body)
            self.assertEqual(resp.status_code, 201, resp.content)
            row = Record.objects.get(hash=resp.json()["hash"])
            self.assertIsNone(row.blob_sha256)
            self.assertEqual(row.raw, body)

    def test_exportbundle_materializes_tiered_bodies(self):
        from commons.bundle import read_bundle

        with override_settings(COMMONS_MAX_RECORD_BYTES=2048, COMMONS_BLOB_DIR=self.blob_dir):
            body = self._big_body()
            self.assertEqual(self._publish(body).status_code, 201)
            out = Path(self.blob_dir) / "tiered.nlb"
            call_command("exportbundle", str(out), stdout=io.StringIO(), stderr=io.StringIO())
            _, records = read_bundle(str(out))
            self.assertIn(body, records, "the bundle carries the real body, not the pointer stub")


# --- provenance anchoring (commons.md open question 2) ---------------------------------------------

class AnchorTests(TestCase):
    """Signed Merkle-root anchors: emitted on root movement, verifiable, served newest-first."""

    def test_disabled_without_seed(self):
        from commons.anchor import record_anchor

        self.assertIsNone(record_anchor())
        resp = Client().get("/v0/anchors")
        self.assertEqual(resp.status_code, 200)
        self.assertFalse(resp.json()["enabled"])
        self.assertEqual(resp.json()["anchors"], [])

    @override_settings(COMMONS_ANCHOR_SEED="anchor-test-seed")
    def test_anchor_signs_the_current_root_and_dedupes(self):
        from commons.anchor import record_anchor
        from commons.bundle import _crypto
        from commons.merkle import set_digest
        from commons.models import Anchor

        Record.objects.create(hash="fn_" + "1" * 64, kind="function-record",
                              schema_version="0.2.0", raw={})
        payload = record_anchor()
        self.assertEqual(payload["root"], set_digest(["fn_" + "1" * 64]))
        self.assertEqual(payload["count"], 1)
        status, producer = _crypto().verify_manifest(payload)
        self.assertEqual(status, "valid")
        self.assertTrue(producer.startswith("did:nova:"))
        self.assertEqual(Anchor.objects.count(), 1)

        # Unchanged root: nothing new. Root moves: a new anchor.
        self.assertIsNone(record_anchor())
        self.assertEqual(Anchor.objects.count(), 1)
        Record.objects.create(hash="fn_" + "2" * 64, kind="function-record",
                              schema_version="0.2.0", raw={})
        second = record_anchor()
        self.assertEqual(second["count"], 2)
        self.assertEqual(Anchor.objects.count(), 2)

        # Tampering with a served anchor is detectable by anyone.
        forged = dict(second)
        forged["count"] = 3
        self.assertEqual(_crypto().verify_manifest(forged)[0], "invalid")

        resp = Client().get("/v0/anchors")
        body = resp.json()
        self.assertTrue(body["enabled"])
        self.assertEqual([a["count"] for a in body["anchors"]], [2, 1], "newest first")


class WitnessTests(TestCase):
    """Anchor cross-node witnessing (open question 2, the federated half): a peer's anchors are
    signature-verified, root-compared against THIS node's corpus, and countersigned."""

    def _origin_anchor(self, hashes, seed="origin-anchor-seed"):
        """A peer's signed anchor over the given record set (signed with the ORIGIN's identity,
        distinct from the witness's own seed)."""
        from commons.bundle import _crypto
        from commons.merkle import set_digest

        return _crypto().sign_manifest(
            {"format_version": "nl-anchor/1", "root": set_digest(hashes), "count": len(hashes),
             "at": "2026-07-14T00:00:00+00:00"}, seed)

    def test_disabled_without_seed(self):
        from commons.witness import witness_peer_anchors

        self.assertEqual(witness_peer_anchors("https://origin.example", []), {"enabled": False})
        resp = Client().get("/v0/witnesses")
        self.assertEqual(resp.status_code, 200)
        self.assertFalse(resp.json()["enabled"])
        self.assertEqual(resp.json()["witnesses"], [])

    @override_settings(COMMONS_ANCHOR_SEED="witness-test-seed")
    def test_countersigns_valid_anchors_and_never_invalid_ones(self):
        from commons.bundle import _crypto
        from commons.models import Witness
        from commons.witness import witness_peer_anchors

        valid = self._origin_anchor(["fn_" + "1" * 64])
        forged = dict(valid)
        forged["count"] = 99  # tampered after signing — must never be countersigned
        summary = witness_peer_anchors("https://origin.example",
                                       [valid, forged, "junk", {"root": "x"}])
        # forged = bad signature, the unsigned dict = also refused (invalid); "junk" = malformed.
        self.assertEqual((summary["witnessed"], summary["invalid"], summary["malformed"]),
                         (1, 2, 1), summary)

        row = Witness.objects.get()
        # Local corpus is empty, the anchored set is not — signature seen, agreement unverified.
        self.assertEqual(row.agreement, "unverified")
        self.assertEqual(row.producer, valid["producer"], "the row indexes the ORIGIN's signer")
        # The witness statement is itself verifiable, embeds the origin anchor VERBATIM, and is
        # signed by a DIFFERENT identity than the origin's — two signatures, no shared honesty.
        status, witness_did = _crypto().verify_manifest(row.payload)
        self.assertEqual(status, "valid")
        self.assertNotEqual(witness_did, valid["producer"])
        self.assertEqual(row.payload["anchor"], valid)
        self.assertEqual(_crypto().verify_manifest(row.payload["anchor"])[0], "valid")

    @override_settings(COMMONS_ANCHOR_SEED="witness-test-seed")
    def test_root_agreement_and_appendonly_upgrade(self):
        from commons.models import Witness
        from commons.witness import witness_peer_anchors

        Record.objects.create(hash="fn_" + "1" * 64, kind="function-record",
                              schema_version="0.2.0", raw={})
        # The origin anchors {fn_1, fn_2}; this node holds only fn_1 → unverified.
        anchor = self._origin_anchor(["fn_" + "1" * 64, "fn_" + "2" * 64])
        s1 = witness_peer_anchors("https://origin.example", [anchor])
        self.assertEqual(s1["witnessed"], 1)
        self.assertEqual(Witness.objects.get().agreement, "unverified")

        # Replication catches up (fn_2 arrives) → the SAME anchor gains a root-matched witness:
        # an APPENDED second statement, the first is never rewritten.
        Record.objects.create(hash="fn_" + "2" * 64, kind="function-record",
                              schema_version="0.2.0", raw={})
        s2 = witness_peer_anchors("https://origin.example", [anchor])
        self.assertEqual(s2["witnessed"], 1, s2)
        rows = list(Witness.objects.order_by("id"))
        self.assertEqual([r.agreement for r in rows], ["unverified", "root-matched"])

        # Idempotent thereafter: root-matched is the terminal state for this (origin, root).
        s3 = witness_peer_anchors("https://origin.example", [anchor])
        self.assertEqual((s3["witnessed"], s3["already"]), (0, 1), s3)
        self.assertEqual(Witness.objects.count(), 2)

    @override_settings(COMMONS_ANCHOR_SEED="witness-test-seed")
    def test_endpoint_serves_newest_first_and_filters_by_origin(self):
        from commons.witness import witness_peer_anchors

        witness_peer_anchors("https://a.example", [self._origin_anchor(["fn_" + "3" * 64])])
        witness_peer_anchors("https://b.example", [self._origin_anchor(["fn_" + "4" * 64])])
        body = Client().get("/v0/witnesses").json()
        self.assertTrue(body["enabled"])
        self.assertEqual([w["origin"] for w in body["witnesses"]],
                         ["https://b.example", "https://a.example"], "newest first")
        filtered = Client().get("/v0/witnesses?origin=https://a.example/").json()
        self.assertEqual([w["origin"] for w in filtered["witnesses"]], ["https://a.example"])

    @override_settings(COMMONS_ANCHOR_SEED="witness-test-seed")
    def test_task_fetches_the_peer_and_witnesses(self):
        from unittest import mock

        from commons import tasks
        from commons.models import Witness

        anchors = {"anchors": [self._origin_anchor(["fn_" + "5" * 64])], "count": 1, "enabled": True}
        with mock.patch.object(tasks, "_get_json", return_value=anchors) as fake:
            summary = tasks.witness_anchors("https://origin.example/")
        self.assertEqual(summary["peer"], "https://origin.example", "trailing slash stripped")
        self.assertEqual(summary["witnessed"], 1, summary)
        fake.assert_called_once_with("https://origin.example/v0/anchors?limit=100")
        self.assertEqual(Witness.objects.count(), 1)


# --- Merkle set reconciliation (commons.md open question 1) ----------------------------------------

@unittest.skipUnless(VALIDATOR.exists(), "nl-validator release binary not built")
class MerkleSyncTests(TestCase):
    """`GET /v0/sync/merkle` + the reconcile_peer anti-entropy walk."""

    def setUp(self):
        self.client = Client()

    def _publish(self, record):
        return self.client.post("/v0/records", data=json.dumps(record),
                                content_type="application/json")

    def test_merkle_unit_semantics(self):
        from commons.merkle import LEAF_LIMIT, merkle_node, set_digest

        # Order-independent digest, the bundle construction.
        self.assertEqual(set_digest(["fn_ab", "fn_cd"]), set_digest(["fn_cd", "fn_ab"]))
        self.assertNotEqual(set_digest(["fn_ab"]), set_digest(["fn_cd"]))

        # 100 synthetic addresses spread over nibbles: the root partitions into children whose
        # counts sum to the total; a leaf-sized prefix returns the address list itself.
        addrs = [f"fn_{i % 16:x}{i:063x}" for i in range(100)]
        root = merkle_node("", addresses=addrs)
        self.assertEqual(root["count"], 100)
        self.assertGreater(root["count"], LEAF_LIMIT)
        self.assertNotIn("hashes", root)
        self.assertEqual(sum(c["count"] for c in root["children"].values()), 100)
        leaf = merkle_node("0", addresses=addrs)
        self.assertEqual(leaf["count"], root["children"]["0"]["count"])
        self.assertEqual(leaf["digest"], root["children"]["0"]["digest"])
        self.assertEqual(len(leaf["hashes"]), leaf["count"])

    def test_merkle_endpoint(self):
        from commons.merkle import set_digest

        rec = _load("map.json")
        self.assertIn(self._publish(rec).status_code, (200, 201))
        resp = self.client.get("/v0/sync/merkle")
        self.assertEqual(resp.status_code, 200)
        node = resp.json()
        hashes = list(Record.objects.values_list("hash", flat=True))
        self.assertEqual(node["digest"], set_digest(hashes))
        self.assertEqual(node["count"], len(hashes))
        self.assertEqual(self.client.get("/v0/sync/merkle?prefix=zz").status_code, 400)

    def test_reconcile_heals_a_divergent_replica(self):
        from commons import tasks
        from commons.merkle import merkle_node

        peer_records = {r["hash"]: r for r in (_load("map.json"), _load("double.v0.2.json"))}

        def fake_get(url, timeout=30):
            if "/v0/sync/merkle" in url:
                prefix = url.rsplit("prefix=", 1)[1] if "prefix=" in url else ""
                return merkle_node(prefix, addresses=list(peer_records))
            for h, r in peer_records.items():
                if h in url:
                    return r
            raise AssertionError(f"unexpected fetch {url}")

        with mock.patch.object(tasks, "_get_json", fake_get):
            out = tasks.reconcile_peer("https://peer.example")
            self.assertFalse(out["in_sync"])
            self.assertEqual(out["mirrored"], 2, out)
            for h in peer_records:
                self.assertTrue(Record.objects.filter(hash=h).exists())
            # Converged: the next round is one equal-digest request and no fetches.
            again = tasks.reconcile_peer("https://peer.example")
            self.assertTrue(again["in_sync"])
            self.assertEqual(again["requests"], 1)
            self.assertEqual(again["mirrored"], 0)

    def test_reconcile_refuses_lying_content(self):
        from commons import tasks
        from commons.merkle import merkle_node

        good = _load("map.json")
        lying_addr = "fn_" + "e" * 64

        def fake_get(url, timeout=30):
            if "/v0/sync/merkle" in url:
                prefix = url.rsplit("prefix=", 1)[1] if "prefix=" in url else ""
                return merkle_node(prefix, addresses=[lying_addr])
            return good  # the peer serves DIFFERENT content under the claimed address

        with mock.patch.object(tasks, "_get_json", fake_get):
            out = tasks.reconcile_peer("https://peer.example")
            self.assertEqual(out["mirrored"], 0, "content under a lying address is refused")
            self.assertFalse(Record.objects.filter(hash=lying_addr).exists())
