"""Offline tests for the OpenAPI ingestion adapter — no network, no live service.

Generates records from the reference item-store description and checks:
  1. every generated record CERTIFIES (typecheck / effects / termination / complexity);
  2. the FAITHFULNESS contract — the generated bodyless-verb bodies are byte-identical to the
     hand-authored GW6 records (`item_status` / `delete_item`), so machine generation reproduces
     what a human wrote from the same description.

The GW10 depth (search-service description + inline specs): local $ref resolution, required
query params (string values through `url_encode`, integer schemas as int params through
`to_string`), header params, apiKey-in-header auth, and the honest refusal boundary
(multipart-only bodies, apiKey-in-query/cookie, http basic, external $refs, cookie params).

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
SEARCH_SPEC = _ADAPTER / "examples" / "search-service.openapi.json"

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
        self.assertEqual(set(self.recs),
                         {"healthcheck", "putitem", "getitemstatus", "deleteitem",
                          "creatething", "createthinglocation", "getlatest", "getlatestlocation"})

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

    def test_header_projection_from_documented_header(self):
        # GW16: createThing's 201 documents a `Location` example -> a second record
        # `createThingLocation : … -> Maybe string` over http_full — the call bound once,
        # status-guarded to the documented 201, map_get of the LOWERCASE name. X-Request-Id
        # declares no example -> refused (no `createthingxrequestid` record).
        self.assertNotIn("createthingxrequestid", self.recs)
        rec = json.load(open(self.recs["createthinglocation"]))
        variants = {v["tag"]: v.get("type") for v in rec["signature"]["type"]["result"]["variants"]}
        self.assertEqual(variants, {"Just": {"kind": "builtin", "name": "string"}, "None": None})
        body = json.dumps(json.load(open(Path(self.tmp) / "body-createthinglocation.json")))
        self.assertIn("http_full", body)
        self.assertEqual(body.count("http_full"), 1, "the call must be bound once (let), not repeated")
        self.assertIn('"location"', body)
        self.assertNotIn('"Location"', body, "map_get key is the canonical lowercase name")
        self.assertEqual(rec["examples"][0]["result"],
                         {"kind": "variant", "tag": "Just",
                          "payload": {"kind": "string", "value": "/things/th_44136fa355b3"}})
        self.assertEqual(rec["signature"]["effects"], ["net.write"])
        # getLatest's documented 307 Location projects too — a redirect target is header data.
        latest = json.load(open(self.recs["getlatestlocation"]))
        self.assertEqual(latest["signature"]["effects"], ["net.read"])
        latest_body = json.dumps(json.load(open(Path(self.tmp) / "body-getlatestlocation.json")))
        self.assertIn('"value": 307', latest_body)

    @unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
    def test_header_projection_alpha_equivalent_to_hand_authored(self):
        # The GW16 faithfulness result, one rung above GW7's byte-identity: the generated
        # createThingLocation and the hand-authored GW14 create_thing differ only in a parameter
        # name (`body` vs `v`), so their canonical NORMAL FORMS coincide — α-equivalence decided
        # solver-free by `normalize`.
        def nf(p):
            r = subprocess.run([str(VALIDATOR), "normalize", "--hash", "--body", str(p)],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            return r.stdout.strip()
        self.assertEqual(nf(Path(self.tmp) / "body-createthinglocation.json"),
                         nf(EXAMPLES / "body-create-thing.json"))


class SearchServiceTest(unittest.TestCase):
    """The GW10 depth over the $ref-factored search-service description."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="nl-openapi-gw10-")
        try:
            oi.main([str(SEARCH_SPEC), "--out", cls.tmp])
        except SystemExit:
            pass
        cls.recs = {p.name.replace(".v0.2.json", ""): p
                    for p in Path(cls.tmp).glob("*.v0.2.json")}
        cls.spec = json.load(open(SEARCH_SPEC))

    def _body(self, name):
        return json.dumps(json.load(open(Path(self.tmp) / f"body-{name}.json")))

    def test_multipart_refused_others_generated(self):
        # uploadArchive is multipart-only -> refused; the two $ref-parameterized reads generate,
        # and getVersion's documented 200 example additionally yields a body-projection record.
        self.assertEqual(set(self.recs), {"searchitems", "getversion", "getversionbody"})

    def test_body_projection_from_documented_example(self):
        # getVersion documents `{"version": "1.0.0"}` on its 200 -> a second record
        # `getVersionBody : … -> Maybe Json` whose body is parse_json over the response body and
        # whose worked example asserts Just(JObj({version: JStr})). searchItems documents no
        # example -> no projection (its live payload is state-dependent; we never guess).
        rec = json.load(open(self.recs["getversionbody"]))
        self.assertEqual(rec["signature"]["type"]["result"]["kind"], "sum")
        tags = {v["tag"] for v in rec["signature"]["type"]["result"]["variants"]}
        self.assertEqual(tags, {"Just", "None"})
        self.assertIn("parse_json", self._body("getversionbody"))
        result = rec["examples"][0]["result"]
        self.assertEqual(result["tag"], "Just")
        self.assertEqual(result["payload"]["tag"], "JObj")
        entries = result["payload"]["payload"]["entries"]
        self.assertEqual(entries, [{"key": "version", "value":
                                    {"kind": "variant", "tag": "JStr",
                                     "payload": {"kind": "string", "value": "1.0.0"}}}])
        # Same surface as the status record: params, effect, auth placeholder.
        status = json.load(open(self.recs["getversion"]))
        self.assertEqual(rec["signature"]["type"]["params"],
                         status["signature"]["type"]["params"])
        self.assertEqual(rec["signature"]["effects"], ["net.read"])

    def test_offline_generation_attaches_no_traces(self):
        # Example traces (GW12) are observations of a LIVE run — the offline path must not invent
        # them, and offline-generated records stay byte-stable (the faithfulness contract).
        for name, rp in self.recs.items():
            for ex in json.load(open(rp))["examples"]:
                self.assertNotIn("trace", ex, name)

    def test_json_value_encoder_shapes(self):
        # The encoder promises exactly what parse_json produces — and refuses floats.
        self.assertEqual(oi._json_to_value(None), {"kind": "variant", "tag": "JNull"})
        self.assertEqual(oi._json_to_value(True)["tag"], "JBool")
        self.assertEqual(oi._json_to_value(3), {"kind": "variant", "tag": "JNum",
                                                "payload": {"kind": "int", "value": 3}})
        self.assertIsNone(oi._json_to_value(1.5))
        self.assertIsNone(oi._json_to_value({"ok": [1.5]}))
        keys = [e["key"] for e in oi._json_to_value({"b": 1, "a": 2})["payload"]["entries"]]
        self.assertEqual(keys, ["a", "b"])  # canonical code-point key order

    @unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
    def test_every_record_certifies(self):
        for name, rp in self.recs.items():
            bp = Path(self.tmp) / f"body-{name}.json"
            r = subprocess.run([str(VALIDATOR), "certify", str(rp), "--body", str(bp),
                                "--records", self.tmp], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"{name} did not certify:\n{r.stdout}\n{r.stderr}")

    def test_query_params_encode_by_schema_type(self):
        # A string query value rides through url_encode; an integer one through to_string
        # (digits are unreserved); names are spec-time literals `?q=` / `&limit=`.
        body = self._body("searchitems")
        self.assertIn("url_encode", body)
        self.assertIn("to_string", body)
        self.assertIn("?q=", body)
        self.assertIn("&limit=", body)

    def test_optional_query_param_omitted(self):
        # `offset` is required:false -> not a record parameter (the minimal documented call).
        rec = json.load(open(self.recs["searchitems"]))
        params = rec["signature"]["type"]["params"]
        self.assertEqual(len(params), 3)  # base, q, limit
        self.assertNotIn("offset", self._body("searchitems"))

    def test_integer_schema_becomes_int_param(self):
        rec = json.load(open(self.recs["searchitems"]))
        params = rec["signature"]["type"]["params"]
        self.assertEqual([p["name"] for p in params], ["string", "string", "int"])

    def test_api_key_auth_and_header_param(self):
        # apiKey-in-header -> the scheme's named header with a secret placeholder; a required
        # header PARAM is a record parameter map_put by its literal name with a VAR value.
        search = self._body("searchitems")
        self.assertIn('"X-Api-Key"', search)
        self.assertIn("{{secret:api_key}}", search)
        version = self._body("getversion")
        self.assertIn('"X-Client-Id"', version)
        self.assertIn("x_client_id", version)  # the variable, not a literal value

    def test_ref_parameters_resolved(self):
        # All parameters in the description are $refs — generation happening at all proves
        # resolution; the query literal shows the resolved parameter NAME, not the ref text.
        self.assertNotIn("$ref", self._body("searchitems"))


class OAuthServiceTest(unittest.TestCase):
    """The GW13 surface: OAuth2 client-credentials over the reports-service description."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="nl-openapi-gw13-")
        try:
            oi.main([str(_ADAPTER / "examples" / "reports-service.openapi.json"), "--out", cls.tmp])
        except SystemExit:
            pass
        cls.recs = {p.name.replace(".v0.2.json", ""): p
                    for p in Path(cls.tmp).glob("*.v0.2.json")}

    def test_client_credentials_generates_interactive_refuses(self):
        # getReportSummary (clientCredentials) compiles — plus its documented-example projection;
        # getMyReport (authorizationCode) refuses: an interactive flow needs a principal the
        # effect boundary cannot supply.
        self.assertEqual(set(self.recs), {"getreportsummary", "getreportsummarybody"})

    def test_oauth_placeholder_is_symbolic(self):
        # The record names the identity {{oauth:<scheme-key>}} — no token URL, no credential,
        # nothing to leak: the description's tokenUrl is run-time operator configuration.
        body = json.dumps(json.load(open(Path(self.tmp) / "body-getreportsummary.json")))
        self.assertIn("Bearer {{oauth:reports_auth}}", body)
        self.assertNotIn("{{secret:", body)
        self.assertNotIn("/token", body)

    @unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
    def test_every_record_certifies(self):
        for name, rp in self.recs.items():
            bp = Path(self.tmp) / f"body-{name}.json"
            r = subprocess.run([str(VALIDATOR), "certify", str(rp), "--body", str(bp),
                                "--records", self.tmp], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"{name} did not certify:\n{r.stdout}\n{r.stderr}")


class RefusalBoundaryTest(unittest.TestCase):
    """Inline descriptions locking each documented refusal."""

    BASE = {"openapi": "3.0.0", "info": {"title": "t", "version": "1"},
            "servers": [{"url": "http://127.0.0.1:1"}]}

    def _walk(self, spec):
        return oi.walk({**self.BASE, **spec}, None)

    def test_api_key_in_query_refused(self):
        built, skipped = self._walk({
            "components": {"securitySchemes": {"k": {"type": "apiKey", "in": "query", "name": "key"}}},
            "security": [{"k": []}],
            "paths": {"/x": {"get": {"operationId": "opA", "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("header value", skipped[0][1])

    def test_http_basic_refused(self):
        built, skipped = self._walk({
            "components": {"securitySchemes": {"b": {"type": "http", "scheme": "basic"}}},
            "security": [{"b": []}],
            "paths": {"/x": {"get": {"operationId": "opB", "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("base64", skipped[0][1])

    def test_external_ref_refused(self):
        built, skipped = self._walk({
            "paths": {"/x": {"get": {"operationId": "opC",
                                     "parameters": [{"$ref": "other.json#/components/parameters/P"}],
                                     "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("$ref", skipped[0][1])

    def test_cookie_param_refused(self):
        built, skipped = self._walk({
            "paths": {"/x": {"get": {"operationId": "opD",
                                     "parameters": [{"name": "sid", "in": "cookie",
                                                     "required": True, "schema": {"type": "string"}}],
                                     "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("cookie", skipped[0][1])

    def test_path_item_level_parameters_merge(self):
        # A path-item-level $ref parameter is shared by the operation (no op-level params at all).
        built, skipped = self._walk({
            "components": {"parameters": {
                "Q": {"name": "q", "in": "query", "required": True, "schema": {"type": "string"}}}},
            "paths": {"/x": {
                "parameters": [{"$ref": "#/components/parameters/Q"}],
                "get": {"operationId": "opE", "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(skipped, [])
        record, body_ast, _ = built[0]
        self.assertEqual(len(record["signature"]["type"]["params"]), 2)  # base, q
        self.assertIn("url_encode", json.dumps(body_ast))


if __name__ == "__main__":
    unittest.main()
