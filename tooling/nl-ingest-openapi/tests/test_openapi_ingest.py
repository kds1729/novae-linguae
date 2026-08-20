"""Tests for the OpenAPI ingestion adapter — no external network; the observation-gate tests
run against the in-repo fake service on localhost, everything else is fully offline.

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

    def test_extending_intent_tags(self):
        # Discovery precision (the GitHub-scale finding): every record carries one tag extending
        # its lead intent with its own snake name, so a precise query addresses it directly and
        # the rank's tag-specificity/name-affinity signals engage under broad ones.
        rec = json.load(open(self.recs["getitemstatus"]))
        self.assertIn("query/lookup/get-item-status", rec["intent_tags"])
        put = json.load(open(self.recs["putitem"]))
        self.assertIn("io/network/http/put-item", put["intent_tags"])
        loc = json.load(open(self.recs["createthinglocation"]))
        self.assertIn("io/network/http/create-thing-location", loc["intent_tags"])

    def test_unauthenticated_op_has_no_secret(self):
        # /health declares `security: []` — no auth header, so no secret placeholder in its body.
        body = json.dumps(json.load(open(Path(self.tmp) / "body-healthcheck.json")))
        self.assertNotIn("secret", body)
        put = json.dumps(json.load(open(Path(self.tmp) / "body-putitem.json")))
        self.assertIn("{{secret:api_token}}", put)

    def test_request_content_type_is_emitted(self):
        # Proposal 01: a body-carrying operation sends the media type its description DECLARES.
        # Without it the record reports certify=OK and is then rejected by any service requiring
        # the header — a generated artifact that cannot perform its own documented call.
        for name in ("putitem", "creatething"):
            body = json.dumps(json.load(open(Path(self.tmp) / f"body-{name}.json")))
            self.assertIn('"Content-Type"', body, f"{name} must send its declared media type")
            self.assertIn('"application/json"', body)

    def test_bodyless_verbs_send_no_content_type(self):
        # The other half: nothing to describe, so nothing is claimed. This is also what keeps the
        # GW6 byte-identity result above intact.
        for name in ("getitemstatus", "deleteitem", "healthcheck"):
            body = json.dumps(json.load(open(Path(self.tmp) / f"body-{name}.json")))
            self.assertNotIn("Content-Type", body)

    def test_ambiguous_request_media_types_note_and_omit(self):
        # More than one declared non-multipart type is a choice the description leaves open, so
        # the adapter sends none and SAYS so — the same contract the non-JSON response note keeps.
        # Guessing `application/json` here would be inventing a promise the description withheld.
        spec = {"openapi": "3.0.0", "info": {"title": "t", "version": "1"},
                "servers": [{"url": "http://127.0.0.1:1"}],
                "paths": {"/things": {"post": {
                    "operationId": "createThing", "security": [],
                    "requestBody": {"required": True, "content": {
                        "application/json": {"schema": {"type": "string"}},
                        "application/xml": {"schema": {"type": "string"}}}},
                    "responses": {"201": {"description": "created"}}}}}}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(skipped, [])
        rec, body, notes = built[0]
        self.assertTrue(any("Content-Type` omitted" in n for n in notes),
                        f"the omission must be explained, not silent; got: {notes}")
        self.assertNotIn("Content-Type", json.dumps(body))

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

    def test_multipart_compiles_to_a_deterministic_form(self):
        # uploadArchive is multipart-only — COMPILED now, not refused: the boundary is a
        # spec-time constant riding in the Content-Type literal, the required string parts
        # (archive, note) become caller parameters in declaration order, the optional `tag`
        # part is omitted (the minimal documented call), and framing is all literal.
        self.assertEqual(set(self.recs),
                         {"searchitems", "getversion", "getversionbody", "uploadarchive"})
        rec = json.load(open(self.recs["uploadarchive"]))
        self.assertEqual([p["name"] for p in rec["signature"]["type"]["params"]],
                         ["string", "string", "string"])  # base, archive, note
        self.assertEqual(rec["signature"]["effects"], ["net.write"])
        self.assertEqual(rec["examples"][0]["result"], {"kind": "int", "value": 201})
        body = self._body("uploadarchive")
        self.assertIn("multipart/form-data; boundary=nl-upload_archive-boundary", body)
        self.assertIn('Content-Disposition: form-data; name=\\"archive\\"', body)
        self.assertIn('Content-Disposition: form-data; name=\\"note\\"', body)
        self.assertIn("--nl-upload_archive-boundary--", body)
        self.assertNotIn('name=\\"tag\\"', body)  # the optional part is not in the form
        # Part VALUES are variables (caller data), never literals.
        ast = json.load(open(Path(self.tmp) / "body-uploadarchive.json"))
        self.assertEqual([p["name"] for p in ast["params"]], ["base", "archive", "note"])

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
        built, skipped, _pending, _report = oi.walk({**self.BASE, **spec}, None)
        return built, skipped

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

    def test_url_ref_refused(self):
        # A URL $ref stays refused regardless of any base directory: no network at ingestion
        # time — the description must be locally complete.
        built, skipped = self._walk({
            "paths": {"/x": {"get": {"operationId": "opC",
                                     "parameters": [{"$ref": "https://example.com/o.json#/P"}],
                                     "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("$ref", skipped[0][1])

    def test_multipart_without_part_properties_refused(self):
        # The old blanket multipart refusal narrowed to its honest core: with no declared part
        # properties there are no spec-time part names to build the form from.
        built, skipped = self._walk({
            "paths": {"/u": {"post": {"operationId": "opU",
                                      "requestBody": {"content": {"multipart/form-data": {
                                          "schema": {"type": "object"}}}},
                                      "responses": {"201": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("part properties", skipped[0][1])

    def test_multipart_without_required_parts_refused(self):
        built, skipped = self._walk({
            "paths": {"/u": {"post": {"operationId": "opV",
                                      "requestBody": {"content": {"multipart/form-data": {
                                          "schema": {"type": "object", "properties": {
                                              "note": {"type": "string"}}}}}},
                                      "responses": {"201": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("required parts", skipped[0][1])

    def test_multipart_non_string_part_refused(self):
        built, skipped = self._walk({
            "paths": {"/u": {"post": {"operationId": "opW",
                                      "requestBody": {"content": {"multipart/form-data": {
                                          "schema": {"type": "object", "required": ["n"],
                                                     "properties": {"n": {"type": "integer"}}}}}},
                                      "responses": {"201": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("not a string", skipped[0][1])

    def test_required_body_fields_refused(self):
        # Finding 3 (evolution/aws-sdk-poc): the synthesized `{}` body cannot supply a field
        # the description's own `required` list demands — the worked example would contradict
        # the description it was generated from, so the operation refuses with the reason
        # (finding 11's family, one verb class over). The repro fixture's shape, inline.
        built, skipped = self._walk({
            "paths": {"/widgets": {"post": {"operationId": "createWidget",
                "requestBody": {"required": True, "content": {"application/json": {
                    "schema": {"type": "object", "required": ["name", "kind"],
                               "properties": {"name": {"type": "string"},
                                              "kind": {"type": "string"}}}}}},
                "responses": {"201": {"description": "created"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("`required` list", skipped[0][1])
        self.assertIn("`name`", skipped[0][1])
        self.assertIn("`kind`", skipped[0][1])

    def test_required_body_schema_ref_refused(self):
        # Real corpora (Smithy/Discovery projections) declare body schemas by $ref into
        # components — the `required` list must be read through the reference.
        built, skipped = self._walk({
            "components": {"schemas": {"W": {"type": "object", "required": ["name"],
                                             "properties": {"name": {"type": "string"}}}}},
            "paths": {"/widgets": {"post": {"operationId": "createWidgetRef",
                "requestBody": {"content": {"application/json": {
                    "schema": {"$ref": "#/components/schemas/W"}}}},
                "responses": {"201": {"description": "created"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("`required` list", skipped[0][1])

    def test_alternative_media_type_admitting_empty_body_compiles(self):
        # The refusal fires only when EVERY declared media type requires fields: offered an
        # alternative type that admits the empty body, the minimal documented call survives
        # (the Content-Type ambiguity is already noted, unchanged).
        built, skipped = self._walk({
            "paths": {"/widgets": {"post": {"operationId": "createWidgetAlt",
                "requestBody": {"content": {
                    "application/json": {"schema": {"type": "object", "required": ["name"],
                                                    "properties": {"name": {"type": "string"}}}},
                    "text/plain": {"schema": {"type": "string"}}}},
                "responses": {"201": {"description": "created"}}}}}})
        self.assertEqual(skipped, [])
        self.assertEqual(len(built), 1)

    def test_cookie_param_refused(self):
        built, skipped = self._walk({
            "paths": {"/x": {"get": {"operationId": "opD",
                                     "parameters": [{"name": "sid", "in": "cookie",
                                                     "required": True, "schema": {"type": "string"}}],
                                     "responses": {"200": {"description": "ok"}}}}}})
        self.assertEqual(built, [])
        self.assertIn("cookie", skipped[0][1])

    def test_relative_file_ref_resolves_and_matches_inline(self):
        # The non-local-$ref pull: a RELATIVE-FILE reference resolves against the spec's own
        # directory, nested refs resolve against the REFERENCED document, and — faithfulness —
        # the generated record is byte-identical to the same description with the ref inlined
        # by hand (the reference is pure factoring, not semantics).
        tmp = tempfile.mkdtemp(prefix="nl-openapi-extref-")
        shared = {"components": {
            "parameters": {"Q": {"name": "q", "in": "query", "required": True,
                                 "schema": {"$ref": "#/components/schemas/Str"}}},
            "schemas": {"Str": {"type": "string"}}}}
        json.dump(shared, open(Path(tmp) / "shared.json", "w"))
        op = {"operationId": "opX", "responses": {"200": {"description": "ok"}}}
        main_spec = {**self.BASE,
                     "paths": {"/x": {"get": {**op,
                                              "parameters": [{"$ref": "shared.json#/components/parameters/Q"}]}}}}
        json.dump(main_spec, open(Path(tmp) / "main.json", "w"))
        spec = oi.load_spec(str(Path(tmp) / "main.json"))
        built, skipped, _, _ = oi.walk(spec, None)
        self.assertEqual(skipped, [])
        record, body_ast, _ = built[0]

        inline_spec = {**self.BASE,
                       "paths": {"/x": {"get": {**op,
                                                "parameters": [{"name": "q", "in": "query", "required": True,
                                                                "schema": {"type": "string"}}]}}}}
        built_inline, _, _, _ = oi.walk(inline_spec, None)
        record_inline, body_inline, _ = built_inline[0]
        self.assertEqual(json.dumps(body_ast, sort_keys=True), json.dumps(body_inline, sort_keys=True))
        self.assertEqual(record["hash"], record_inline["hash"])

    def test_directory_escaping_file_ref_refused(self):
        # A file ref may not climb out of the spec's directory — the description is the unit
        # of trust; it does not get to read the rest of the filesystem.
        outer = tempfile.mkdtemp(prefix="nl-openapi-escape-")
        inner = Path(outer) / "spec"
        inner.mkdir()
        json.dump({"components": {"parameters": {"P": {"name": "p", "in": "query",
                                                       "required": True,
                                                       "schema": {"type": "string"}}}}},
                  open(Path(outer) / "outside.json", "w"))
        main_spec = {**self.BASE,
                     "paths": {"/x": {"get": {"operationId": "opY",
                                              "parameters": [{"$ref": "../outside.json#/components/parameters/P"}],
                                              "responses": {"200": {"description": "ok"}}}}}}
        json.dump(main_spec, open(inner / "main.json", "w"))
        spec = oi.load_spec(str(inner / "main.json"))
        built, skipped, _, _ = oi.walk(spec, None)
        self.assertEqual(built, [])
        self.assertIn("$ref", skipped[0][1])

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


class SchemaDerivedTest(unittest.TestCase):
    """Schema-derived projections (the Frankfurter finding: real descriptions declare response
    SCHEMAS, not examples). Offline, a declared schema yields only PENDING projections — a schema
    promises shape, not a value, so no record exists without the live observation gate. The gated
    half runs against the in-repo fake service on localhost (hermetic — no external network)."""

    BASE = {"openapi": "3.0.0", "info": {"title": "t", "version": "1"}}

    @staticmethod
    def _health_spec(base_url, schema):
        return {**SchemaDerivedTest.BASE, "servers": [{"url": base_url}],
                "paths": {"/health": {"get": {
                    "operationId": "getHealth", "security": [],
                    "responses": {"200": {"description": "ok", "content": {
                        "application/json": {"schema": schema}}}}}}}}

    def test_schema_yields_pending_not_records(self):
        # No example anywhere: the base status record builds, the projections stay PENDING
        # (typed per declared property, numeric skipped, required threaded through).
        spec = self._health_spec("http://127.0.0.1:1", {
            "type": "object",
            "properties": {"status": {"type": "string"}, "ready": {"type": "boolean"},
                           "detail": {"type": "object"}, "pid": {"type": "integer"}},
            "required": ["status"]})
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(skipped, [])
        self.assertEqual([r["name_hints"][0] for r, _, _ in built], ["gethealth"])
        by_name = {p["name"]: p for p in pending}
        # pid (integer) is NOT pending: JNum carries int or float — no sound narrowing.
        self.assertEqual(set(by_name), {"getHealthBody", "getHealthStatus",
                                        "getHealthReady", "getHealthDetail"})
        self.assertEqual(by_name["getHealthStatus"]["type_ast"]["result"], oi.MAYBE_STRING)
        self.assertEqual(by_name["getHealthReady"]["type_ast"]["result"], oi.MAYBE_BOOL)
        self.assertEqual(by_name["getHealthDetail"]["type_ast"]["result"], oi.MAYBE_JSON)
        self.assertTrue(by_name["getHealthStatus"]["required_field"])
        self.assertFalse(by_name["getHealthReady"]["required_field"])

    def test_suffixed_json_content_type_licenses_schema(self):
        # RFC 6839 structured-syntax suffixes (the NWS finding): `application/ld+json` (and
        # geo+json/hal+json, with parameters) IS the parses-as-JSON promise — the schema-derived
        # path treats it exactly like application/json. A non-JSON type still yields nothing.
        spec = self._health_spec("http://127.0.0.1:1", {
            "type": "object", "properties": {"status": {"type": "string"}},
            "required": ["status"]})
        resp = spec["paths"]["/health"]["get"]["responses"]["200"]
        resp["content"] = {"application/ld+json; charset=utf-8":
                           resp["content"].pop("application/json")}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual({p["name"] for p in pending}, {"getHealthBody", "getHealthStatus"})
        resp["content"] = {"text/html": {"schema": {"type": "string"}}}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(pending, [], "a non-JSON content type licenses nothing")
        self.assertTrue(any("text/html" in n for n in built[0][2]),
                        "and it SAYS so — a declined body is an explicit refusal, not a silence")

    def test_media_range_response_is_noted_not_silent(self):
        # A media RANGE (`*/*`) is what a Swagger 2.0 description WITHOUT `produces` converts to
        # (swagger2openapi has no media type to key the response content by), so it is the common
        # real-world shape, not an exotic one. `*/*` promises ANY type, so declining to project is
        # correct — it cannot license the parses-as-JSON promise. Declining SILENTLY is the defect:
        # the status record builds, certifies, and reports nothing, so a consumer cannot tell the
        # projections were dropped from a description that never promised a body at all.
        spec = self._health_spec("http://127.0.0.1:1", {
            "type": "object", "properties": {"status": {"type": "string"}},
            "required": ["status"]})
        resp = spec["paths"]["/health"]["get"]["responses"]["200"]
        resp["content"] = {"*/*": resp["content"].pop("application/json")}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(skipped, [])
        self.assertEqual(pending, [], "`*/*` promises any type — it licenses no projection")
        self.assertEqual([r["name_hints"][0] for r, _, _ in built], ["gethealth"],
                         "the status record still builds — only the projections are declined")
        notes = built[0][2]
        self.assertTrue(any("not projected" in n and "*/*" in n for n in notes),
                        f"the declined body must be explained, not silent; got: {notes}")

    def test_bodyless_response_stays_silent(self):
        # The other half of the contract: a response that declares NO body has nothing to refuse,
        # so the note must NOT fire. Silence is correct here.
        spec = {**self.BASE, "servers": [{"url": "http://127.0.0.1:1"}],
                "paths": {"/health": {"get": {
                    "operationId": "getHealth", "security": [],
                    "responses": {"204": {"description": "no content"}}}}}}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(pending, [])
        self.assertFalse(any("not projected" in n for n in built[0][2]),
                         "a description that promised no body must not be reported as declined")

    def test_json_response_emits_no_nonjson_note(self):
        # The control for the two above: a concrete JSON type projects, so the non-JSON refusal
        # must not fire alongside the pending-projection note.
        spec = self._health_spec("http://127.0.0.1:1", {
            "type": "object", "properties": {"status": {"type": "string"}},
            "required": ["status"]})
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual({p["name"] for p in pending}, {"getHealthBody", "getHealthStatus"})
        self.assertFalse(any("not JSON" in n for n in built[0][2]))

    def test_documented_example_wins_over_schema(self):
        # A response documenting BOTH an example and a schema takes the example path (spec-time
        # value, no live gate needed) — the schema path exists for the example-less reality.
        spec = self._health_spec("http://127.0.0.1:1", {"type": "object"})
        media = spec["paths"]["/health"]["get"]["responses"]["200"]["content"]["application/json"]
        media["example"] = {"status": "ok"}
        built, skipped, pending, _report = oi.walk(spec, None)
        self.assertEqual(pending, [])
        self.assertIn("gethealthbody", [r["name_hints"][0] for r, _, _ in built])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
class SchemaObservationGateTest(unittest.TestCase):
    """The live half: the observation gate materializes schema-derived records against the
    in-repo fake service (GET /health answers {"status":"ok"}), and a description the service
    does not honor REFUSES to publish."""

    PORT = 18878

    @classmethod
    def setUpClass(cls):
        import time
        import urllib.request
        cls.base = f"http://127.0.0.1:{cls.PORT}"
        cls.svc = subprocess.Popen(
            [sys.executable, str(REPO_ROOT / "tooling" / "fake-service" / "fake_service.py"),
             "--port", str(cls.PORT)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(50):
            try:
                urllib.request.urlopen(f"{cls.base}/health", timeout=0.2)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError("fake service did not come up")

    @classmethod
    def tearDownClass(cls):
        cls.svc.terminate()
        cls.svc.wait()

    def _ingest(self, schema, tag, extra=()):
        tmp = tempfile.mkdtemp(prefix=f"nl-openapi-schema-{tag}-")
        sp = Path(tmp) / "spec.json"
        json.dump(SchemaDerivedTest._health_spec(self.base, schema), open(sp, "w"))
        code = 0
        try:
            oi.main([str(sp), "--out", tmp, "--verify-against", self.base, *extra])
        except SystemExit as e:
            code = e.code
        return tmp, code

    def test_observation_materializes_and_replays(self):
        # An honest schema: one live execution supplies each record's worked example
        # (trace-attached), certify passes, and `run` replays it offline.
        tmp, code = self._ingest({"type": "object",
                                  "properties": {"status": {"type": "string"}},
                                  "required": ["status"]}, "ok")
        self.assertEqual(code, 0)
        rec = json.load(open(Path(tmp) / "gethealthstatus.v0.2.json"))
        self.assertIn("parse/get-health-status", rec["intent_tags"])
        ex = rec["examples"][0]
        self.assertEqual(ex["result"], {"kind": "variant", "tag": "Just",
                                        "payload": {"kind": "string", "value": "ok"}})
        self.assertTrue(ex["trace"].startswith("trc_"))
        body = json.load(open(Path(tmp) / "body-gethealthstatus.json"))
        self.assertIn('"JStr"', json.dumps(body))
        r = subprocess.run([str(VALIDATOR), "run", str(Path(tmp) / "gethealthstatus.v0.2.json"),
                            "--records", tmp], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_documented_example_blobifies_without_the_live_gate(self):
        # The write path must not depend on --verify-against: a large DOCUMENTED example (the
        # description carries the payload, no live gate involved) also goes by address, and the
        # record is re-addressed to match. Certify + offline example check still pass from the
        # emitted sidecar.
        import hashlib
        tmp = tempfile.mkdtemp(prefix="nl-openapi-docblob-")
        spec = SchemaDerivedTest._health_spec("http://127.0.0.1:1", {"type": "object"})
        media = spec["paths"]["/health"]["get"]["responses"]["200"]["content"]["application/json"]
        media["example"] = {"status": "x" * 300}
        sp = Path(tmp) / "spec.json"
        json.dump(spec, open(sp, "w"))
        code = 0
        try:
            oi.main([str(sp), "--out", tmp, "--blob-threshold", "64"])
        except SystemExit as e:
            code = e.code
        self.assertEqual(code, 0)
        rec = json.load(open(Path(tmp) / "gethealthbody.v0.2.json"))
        ex = rec["examples"][0]
        self.assertNotIn("result", ex)
        sha = ex["result_blob"]["sha256"]
        raw = (Path(tmp) / f"blob-{sha}.json").read_bytes()
        self.assertEqual(hashlib.sha256(raw).hexdigest(), sha)
        # The record was re-addressed after blobifying: its embedded hash must verify. (No offline
        # example replay here — an effectful documented-example record carries no trace without
        # the live gate; that is the pre-GW12 reality, not a blob property.)
        r = subprocess.run([str(VALIDATOR), "verify", str(Path(tmp) / "gethealthbody.v0.2.json")],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr or r.stdout)

    def test_oversized_example_value_goes_by_address(self):
        # Above --blob-threshold the observed expected value leaves the record: a result_blob
        # pointer (sha256 + bytes) plus a blob-<sha256>.json sidecar of the JCS-canonical
        # value-expression bytes (the NWS 413 rung — a multi-MB observed document must not blow
        # the node's record-store cap). The record still certifies and `run` still replays the
        # example offline, resolving the blob from the records dir; a corrupted blob is refused.
        import hashlib
        tmp, code = self._ingest({"type": "object",
                                  "properties": {"status": {"type": "string"}},
                                  "required": ["status"]}, "blob",
                                 extra=["--blob-threshold", "16"])
        self.assertEqual(code, 0)
        rec = json.load(open(Path(tmp) / "gethealthbody.v0.2.json"))
        ex = rec["examples"][0]
        self.assertNotIn("result", ex, "oversized value must not stay inline")
        sha = ex["result_blob"]["sha256"]
        sidecar = Path(tmp) / f"blob-{sha}.json"
        raw = sidecar.read_bytes()
        self.assertEqual(hashlib.sha256(raw).hexdigest(), sha)
        self.assertEqual(ex["result_blob"]["bytes"], len(raw))
        # The blob IS the expected value-expression: the observed Just(JObj …) document.
        val = json.loads(raw)
        self.assertEqual(val["tag"], "Just")
        # Offline replay resolves the blob from the records dir (already checked by main's own
        # replay pass, but assert it directly for the record that went by address).
        r = subprocess.run([str(VALIDATOR), "run", str(Path(tmp) / "gethealthbody.v0.2.json"),
                            "--records", tmp], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        # Hash-is-the-truth: content that no longer matches the sidecar's name is refused.
        sidecar.write_bytes(raw + b" ")
        r = subprocess.run([str(VALIDATOR), "run", str(Path(tmp) / "gethealthbody.v0.2.json"),
                            "--records", tmp], capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("corrupted blob", (r.stderr or "") + (r.stdout or ""))

    def test_lying_required_property_refuses(self):
        # The description promises a required property the service never answers: the observed
        # document violates the declared schema -> the gate FAILS, nothing materializes.
        tmp, code = self._ingest({"type": "object",
                                  "properties": {"uptime": {"type": "string"}},
                                  "required": ["uptime"]}, "lie")
        self.assertEqual(code, 1)
        self.assertFalse((Path(tmp) / "gethealthbody.v0.2.json").exists())
        self.assertFalse((Path(tmp) / "gethealthuptime.v0.2.json").exists())

    def test_lying_property_type_refuses(self):
        # The service answers status as a string; a description declaring it boolean is held to
        # its own words — whole-document conformance AND the required-field narrowing both fail.
        tmp, code = self._ingest({"type": "object",
                                  "properties": {"status": {"type": "boolean"}},
                                  "required": ["status"]}, "type")
        self.assertEqual(code, 1)
        self.assertFalse((Path(tmp) / "gethealthbody.v0.2.json").exists())
        self.assertFalse((Path(tmp) / "gethealthstatus.v0.2.json").exists())

    def test_failed_call_never_mints_an_optional_projection(self):
        # FINDING 8 (evolution/gcp-sdk-poc): a constructible GET whose call FAILS (here: the
        # service requires auth the spec doesn't declare, so 401) must not materialise ANY
        # projection — including the optional-field ones the `required` guard never covered
        # (Discovery-derived descriptions declare no `required` at all). Every projection's own
        # recorded observation is held to the declared status: no document, nothing observed.
        spec = {"openapi": "3.0.0", "info": {"title": "f8", "version": "1"},
                "servers": [{"url": self.base}],
                "paths": {"/items/definitely-absent": {"get": {
                    "operationId": "getAbsent", "security": [],
                    "responses": {"200": {"description": "the item",
                        "content": {"application/json": {"schema": {
                            "type": "object",
                            "properties": {"alpha": {"type": "string"},
                                           "beta": {"type": "string"}}}}}}}}}}}
        tmp = tempfile.mkdtemp(prefix="nl-openapi-schema-f8-")
        sp = Path(tmp) / "spec.json"
        json.dump(spec, open(sp, "w"))
        code = 0
        try:
            oi.main([str(sp), "--out", tmp, "--verify-against", self.base])
        except SystemExit as e:
            code = e.code
        self.assertEqual(code, 1)
        for name in ("getabsentbody", "getabsentalpha", "getabsentbeta"):
            self.assertFalse((Path(tmp) / f"{name}.v0.2.json").exists(),
                             f"{name} materialised off a failed call")

    def test_optional_none_with_obtained_document_still_materialises(self):
        # The other half of finding 8's distinction: when the declared document WAS obtained and
        # an optional field is genuinely absent, `None` is a legitimate observation — the record
        # materialises, trace-attached, and replays offline.
        tmp, code = self._ingest({"type": "object",
                                  "properties": {"status": {"type": "string"},
                                                 "uptime": {"type": "string"}}}, "optabsent")
        self.assertEqual(code, 0)
        up = json.load(open(Path(tmp) / "gethealthuptime.v0.2.json"))
        ex = up["examples"][0]
        self.assertEqual(ex["result"], {"kind": "variant", "tag": "None"})
        self.assertTrue(ex["trace"].startswith("trc_"))
        st = json.load(open(Path(tmp) / "gethealthstatus.v0.2.json"))
        self.assertEqual(st["examples"][0]["result"]["tag"], "Just")


class ObserveArgTest(unittest.TestCase):
    """Operator-supplied observation arguments (--observe-arg — the answer to
    evolution/gcp-sdk-poc open questions 1+2): a path-parameter GET becomes eligible for
    schema-derived projections when the operator names the server state the description
    cannot; the observation is read-only by rule, and every binding that cannot be honored
    refuses loudly with the reason, before any artifact is written or any live call made."""

    PORT = 18879

    @classmethod
    def setUpClass(cls):
        import time
        import urllib.request
        cls.base = f"http://127.0.0.1:{cls.PORT}"
        cls.svc = subprocess.Popen(
            [sys.executable, str(REPO_ROOT / "tooling" / "fake-service" / "fake_service.py"),
             "--port", str(cls.PORT)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(50):
            try:
                urllib.request.urlopen(f"{cls.base}/health", timeout=0.2)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError("fake service did not come up")

    @classmethod
    def tearDownClass(cls):
        cls.svc.terminate()
        cls.svc.wait()

    @staticmethod
    def _get_item_op(extra_params=(), responses=None):
        return {"operationId": "getItem",
                "parameters": [{"name": "name", "in": "path", "required": True,
                                "schema": {"type": "string"}}, *extra_params],
                "responses": responses or {
                    "200": {"description": "the stored item",
                            "content": {"application/json": {"schema": {
                                "type": "object",
                                "properties": {"kind": {"type": "string"}},
                                "required": ["kind"]}}}},
                    "404": {"description": "absent"}}}

    @classmethod
    def _spec(cls, base, ops):
        return {"openapi": "3.0.0", "info": {"title": "t", "version": "1"},
                "servers": [{"url": base}],
                "components": {"securitySchemes": {
                    "api_token": {"type": "http", "scheme": "bearer"}}},
                "security": [{"api_token": []}],
                "paths": {"/items/{name}": ops}}

    def _main(self, spec, extra):
        tmp = tempfile.mkdtemp(prefix="nl-openapi-observe-")
        sp = Path(tmp) / "spec.json"
        json.dump(spec, open(sp, "w"))
        code = 0
        try:
            oi.main([str(sp), "--out", tmp, *extra])
        except SystemExit as e:
            code = e.code
        return tmp, code

    def test_observation_at_operator_arguments_materializes_and_replays(self):
        # The headline: GET /items/{name} is inadmissible under the constructibility rule (the
        # path parameter names server state), but the OPERATOR knows a name — the observation
        # runs once at the supplied arguments, the observed document is held to the declared
        # schema, and the projection records exist with the operator's values visible in the
        # example. The leaf record's own example stays the spec-derived absent-name 404 probe.
        import urllib.request
        req = urllib.request.Request(f"{self.base}/items/oq1-widget",
                                     data=b'{"kind": "widget"}', method="PUT",
                                     headers={"Authorization": "Bearer test-token"})
        urllib.request.urlopen(req, timeout=5)
        tmp, code = self._main(self._spec(self.base, {"get": self._get_item_op()}),
                               ["--verify-against", self.base,
                                "--observe-arg", "getItem.name=oq1-widget"])
        self.assertEqual(code, 0)
        body_rec = json.load(open(Path(tmp) / "getitembody.v0.2.json"))
        ex = body_rec["examples"][0]
        self.assertEqual(ex["args"], [{"kind": "string", "value": self.base},
                                      {"kind": "string", "value": "oq1-widget"}])
        self.assertEqual(ex["result"]["tag"], "Just")
        self.assertTrue(ex["trace"].startswith("trc_"))
        kind_rec = json.load(open(Path(tmp) / "getitemkind.v0.2.json"))
        self.assertEqual(kind_rec["examples"][0]["result"],
                         {"kind": "variant", "tag": "Just",
                          "payload": {"kind": "string", "value": "widget"}})
        # Finding 11's constructive half: the LEAF example also takes the operator's arguments
        # and asserts the documented success — real, reachable, live-checked above.
        leaf = json.load(open(Path(tmp) / "getitem.v0.2.json"))
        self.assertEqual(leaf["examples"][0]["args"][1], {"kind": "string", "value": "oq1-widget"})
        self.assertEqual(leaf["examples"][0]["result"], {"kind": "int", "value": 200})
        r = subprocess.run([str(VALIDATOR), "run", str(Path(tmp) / "getitembody.v0.2.json"),
                            "--records", tmp], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_mutating_verb_refuses(self):
        # Read-only by rule (open question 2): an observation must not create server state
        # during ingestion. The refusal happens before any artifact or live call.
        spec = self._spec("http://127.0.0.1:1", {
            "put": {"operationId": "putItem",
                    "parameters": [{"name": "name", "in": "path", "required": True,
                                    "schema": {"type": "string"}}],
                    "requestBody": {"required": True, "content": {
                        "application/json": {"schema": {"type": "object"}}}},
                    "responses": {"201": {"description": "created",
                                          "content": {"application/json": {"schema": {
                                              "type": "object"}}}}}}})
        tmp, code = self._main(spec, ["--verify-against", "http://127.0.0.1:1",
                                      "--observe-arg", "putItem.name=x"])
        self.assertIn("read-only", str(code))
        self.assertEqual(list(Path(tmp).glob("*.v0.2.json")), [])

    def test_unbound_path_parameter_refuses(self):
        op = self._get_item_op(extra_params=[{"name": "verbose", "in": "query",
                                              "required": True,
                                              "schema": {"type": "string"}}])
        tmp, code = self._main(self._spec("http://127.0.0.1:1", {"get": op}),
                               ["--verify-against", "http://127.0.0.1:1",
                                "--observe-arg", "getItem.verbose=yes"])
        self.assertIn("path parameter(s) unbound: `name`", str(code))

    def test_unknown_parameter_binding_refuses(self):
        tmp, code = self._main(self._spec("http://127.0.0.1:1", {"get": self._get_item_op()}),
                               ["--verify-against", "http://127.0.0.1:1",
                                "--observe-arg", "getItem.name=x",
                                "--observe-arg", "getItem.color=red"])
        self.assertIn("binding names no declared required parameter: `color`", str(code))

    def test_unknown_operation_refuses(self):
        tmp, code = self._main(self._spec("http://127.0.0.1:1", {"get": self._get_item_op()}),
                               ["--verify-against", "http://127.0.0.1:1",
                                "--observe-arg", "noSuchOp.name=x"])
        self.assertIn("no such operationId", str(code))

    def test_binding_a_schemaless_get_supplies_the_leaf_example(self):
        # A binding on a GET with no declared response schema is still consumed — it supplies
        # the STATUS record's worked example (finding 11's constructive half): the operator's
        # arguments, asserting the documented success, live-checked against the real item.
        import urllib.request
        req = urllib.request.Request(f"{self.base}/items/oq1-leaf",
                                     data=b'{"kind": "leaf"}', method="PUT",
                                     headers={"Authorization": "Bearer test-token"})
        urllib.request.urlopen(req, timeout=5)
        op = self._get_item_op(responses={"200": {"description": "no body promised"},
                                          "404": {"description": "absent"}})
        tmp, code = self._main(self._spec(self.base, {"get": op}),
                               ["--verify-against", self.base,
                                "--observe-arg", "getItem.name=oq1-leaf"])
        self.assertEqual(code, 0)
        leaf = json.load(open(Path(tmp) / "getitem.v0.2.json"))
        self.assertEqual(leaf["examples"][0]["args"][1], {"kind": "string", "value": "oq1-leaf"})
        self.assertEqual(leaf["examples"][0]["result"], {"kind": "int", "value": 200})

    def test_pathparam_get_without_documented_404_refuses(self):
        # Finding 11: the absent-name convention would assert a documented success at a name
        # deliberately chosen to be absent — false by construction (Discovery documents only
        # success). Without a binding the operation is REFUSED with the reason, not compiled
        # with an unsatisfiable example; certify cannot catch it, so generation must.
        op = self._get_item_op(responses={"200": {"description": "the item",
            "content": {"application/json": {"schema": {"type": "object"}}}}})
        tmp, code = self._main(self._spec("http://127.0.0.1:1", {"get": op}), [])
        self.assertNotEqual(code, 0)
        out_records = list(Path(tmp).glob("*.v0.2.json"))
        self.assertEqual(out_records, [], "an unsatisfiable example must not be written")

    def test_requires_verify_against(self):
        tmp, code = self._main(self._spec("http://127.0.0.1:1", {"get": self._get_item_op()}),
                               ["--observe-arg", "getItem.name=x"])
        self.assertIn("requires --verify-against", str(code))

    def test_binding_grammar_rightmost_dot(self):
        # operationIds may themselves contain dots (Google Discovery ids): the RIGHTMOST dot
        # before `=` splits opId from param.
        self.assertEqual(oi.parse_observe_args(["storage.buckets.get.bucket=b1"]),
                         {"storage.buckets.get": {"bucket": "b1"}})


class TokenBindingTests(unittest.TestCase):
    """parse_tokens / bound_secrets — the per-scheme credential grammar (offline)."""

    def test_default_is_historical_single_token(self):
        mapping, default = oi.parse_tokens(None)
        self.assertEqual((mapping, default), ({}, "test-token"))
        self.assertEqual(oi.bound_secrets(["a", "b"], mapping, default),
                         [("a", "test-token"), ("b", "test-token")])

    def test_name_bindings_and_bare_default_compose(self):
        mapping, default = oi.parse_tokens(["keyAuth=tok-key", "fallback-tok"])
        self.assertEqual(mapping, {"keyAuth": "tok-key"})
        self.assertEqual(default, "fallback-tok")
        self.assertEqual(oi.bound_secrets(["keyAuth", "other"], mapping, default),
                         [("keyAuth", "tok-key"), ("other", "fallback-tok")])

    def test_unbound_scheme_without_default_is_omitted(self):
        mapping, default = oi.parse_tokens(["keyAuth=tok-key"])
        self.assertIsNone(default)
        self.assertEqual(oi.bound_secrets(["keyAuth", "other"], mapping, default),
                         [("keyAuth", "tok-key")])

    def test_two_bare_tokens_refuse(self):
        with self.assertRaises(SystemExit):
            oi.parse_tokens(["one", "two"])


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
class MultiSchemeAuthGateTest(unittest.TestCase):
    """The one-token-for-all-schemes limit, closed: a description mixing bearer and apiKey
    live-gates in ONE run when each scheme gets its own --token binding — and the old behavior
    (a single shared value) demonstrably fails the scheme it shortchanges."""

    PORT = 18879
    BEARER, KEY = "tok-bearer", "tok-key"

    @classmethod
    def setUpClass(cls):
        import time
        import urllib.request
        cls.base = f"http://127.0.0.1:{cls.PORT}"
        cls.svc = subprocess.Popen(
            [sys.executable, str(REPO_ROOT / "tooling" / "fake-service" / "fake_service.py"),
             "--port", str(cls.PORT), "--token", cls.BEARER, "--api-key", cls.KEY],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(50):
            try:
                urllib.request.urlopen(f"{cls.base}/health", timeout=0.2)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError("fake service did not come up")

    @classmethod
    def tearDownClass(cls):
        cls.svc.terminate()
        cls.svc.wait()

    def _spec(self):
        return {
            "openapi": "3.0.0", "info": {"title": "two-scheme", "version": "1"},
            "servers": [{"url": self.base}],
            "components": {"securitySchemes": {
                "itemsAuth": {"type": "http", "scheme": "bearer"},
                "keyAuth": {"type": "apiKey", "in": "header", "name": "X-Api-Key"}}},
            "paths": {
                "/items/{name}": {"get": {
                    "operationId": "getItemStatus", "security": [{"itemsAuth": []}],
                    "parameters": [{"name": "name", "in": "path", "required": True,
                                    "schema": {"type": "string"}}],
                    "responses": {"200": {"description": "found"},
                                  "404": {"description": "absent"}}}},
                "/search": {"get": {
                    "operationId": "searchItems", "security": [{"keyAuth": []}],
                    "parameters": [
                        {"name": "q", "in": "query", "required": True,
                         "schema": {"type": "string"}},
                        {"name": "limit", "in": "query", "required": True,
                         "schema": {"type": "integer"}}],
                    "responses": {"200": {"description": "results"}}}},
            },
        }

    def _ingest(self, tokens):
        tmp = tempfile.mkdtemp(prefix="nl-openapi-multischeme-")
        sp = Path(tmp) / "spec.json"
        json.dump(self._spec(), open(sp, "w"))
        code = 0
        try:
            oi.main([str(sp), "--out", tmp, "--verify-against", self.base, *tokens])
        except SystemExit as e:
            code = e.code
        return tmp, code

    def test_per_scheme_bindings_gate_both_ops_in_one_run(self):
        tmp, code = self._ingest(["--token", f"itemsAuth={self.BEARER}",
                                  "--token", f"keyAuth={self.KEY}"])
        self.assertEqual(code, 0)
        for name in ("getitemstatus", "searchitems"):
            rec = json.load(open(Path(tmp) / f"{name}.v0.2.json"))
            self.assertTrue(rec["examples"][0]["trace"].startswith("trc_"), name)
        # Each record carries ITS OWN scheme's placeholder — not one shared secret.
        items = json.dumps(json.load(open(Path(tmp) / "body-getitemstatus.json")))
        search = json.dumps(json.load(open(Path(tmp) / "body-searchitems.json")))
        self.assertIn("{{secret:itemsAuth}}", items)
        self.assertIn("{{secret:keyAuth}}", search)

    def test_single_shared_token_fails_the_other_scheme(self):
        # The pre-close behavior, now an honest failure: the bearer value reused for the apiKey
        # scheme draws the service's 401 and the live gate refuses that record.
        tmp, code = self._ingest(["--token", self.BEARER])
        self.assertNotEqual(code, 0)

    def test_binding_an_undeclared_scheme_refuses(self):
        tmp, code = self._ingest(["--token", "nosuch=x"])
        self.assertNotEqual(code, 0)


if __name__ == "__main__":
    unittest.main()
