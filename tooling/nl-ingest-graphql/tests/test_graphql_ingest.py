"""Tests for nl-ingest-graphql.

Offline: the schema -> pending-projection synthesis (documents, variables, selection sets, types,
effects, the refusal boundary) needs no service. Live: the observation gate against the in-repo
fake service's `/graphql` (both transports, absent name, auth, a violated promise), each record
certified by the built `nl-validator` and replayed offline with no secrets.

    /home/claude/sandbox/ft-venv/bin/python -m unittest discover -s tests
"""

import json
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_ADAPTER = _HERE.parent
REPO_ROOT = _ADAPTER.parent.parent
VALIDATOR = REPO_ROOT / "tooling" / "validator" / "target" / "release" / "nl-validator"
FAKE = REPO_ROOT / "tooling" / "fake-service" / "fake_service.py"
SCHEMA = _ADAPTER / "examples" / "item-store.graphql.json"

sys.path.insert(0, str(_ADAPTER))
sys.path.insert(0, str(REPO_ROOT / "tooling" / "fake-service"))
import graphql_ingest as gi  # noqa: E402


def _schema():
    return gi.load_schema(str(SCHEMA))


def _pending(name, **kw):
    schema = _schema()
    tindex = gi.type_index(schema)
    field = next(f for f in tindex["Query"]["fields"] if f["name"] == name)
    return gi.build_field(tindex, field, **kw)


class SynthesisTest(unittest.TestCase):
    def test_example_schema_is_the_fake_services(self):
        import fake_service
        self.assertEqual(json.load(open(SCHEMA))["data"]["__schema"], fake_service.gql_introspection())

    def test_argless_field_is_constructible_and_read_only(self):
        st, pending, notes = _pending("health")
        self.assertEqual(st, "ok")
        whole, status = pending
        self.assertEqual(whole["name"], "health")
        self.assertEqual(whole["document"], "query Q { health { status } }")
        self.assertEqual(whole["effect"], "net.read")
        self.assertEqual(whole["args"], [{"kind": "string", "value": "{{base}}"}])
        self.assertEqual(whole["type_ast"]["result"], gi.MAYBE_JSON)
        self.assertEqual(status["name"], "healthStatus")
        self.assertEqual(status["type_ast"]["result"], gi.MAYBE_STRING)
        self.assertTrue(status["required"])  # Health! . status: String!
        self.assertEqual(whole["intent"][-1], "query/lookup/health")
        self.assertEqual(status["intent"][-1], "parse/health-status")

    def test_get_transport_puts_the_document_in_the_url_and_variables_through_url_encode(self):
        st, pending, _ = _pending("item")
        body = json.dumps(pending[0]["body_ast"])
        self.assertIn("?query=query%20Q%28%24name%3A%20ID%21%29", body)  # spec-time percent-encoded
        self.assertIn('"name": "url_encode"', body)
        self.assertIn('"name": "render_json"', body)
        self.assertIn('"tag": "JObj"', body)
        self.assertIn('"tag": "JStr"', body)  # the ID argument as a Json string value
        self.assertNotIn("Content-Type", body)

    def test_post_transport_is_a_json_body_and_pays_the_method_rule(self):
        st, pending, _ = _pending("item", transport="post")
        whole = pending[0]
        self.assertEqual(whole["effect"], "net.write")
        body = json.dumps(whole["body_ast"])
        self.assertIn('"value": "POST"', body)
        self.assertIn("application/json", body)
        self.assertNotIn("url_encode", body)

    def test_required_argument_becomes_a_typed_parameter_in_declared_order(self):
        st, pending, _ = _pending("item")
        self.assertEqual(pending[0]["type_ast"]["params"], [gi.STRING, gi.STRING])
        self.assertEqual(pending[0]["body_ast"]["params"], [{"name": "base"}, {"name": "name"}])
        self.assertIsNone(pending[0]["args"])  # needs --observe-arg
        self.assertEqual(pending[0]["document"],
                         "query Q($name: ID!) { item(name: $name) { name size tags note kind fresh } }")

    def test_selection_is_scalar_leaves_only_at_depth_1_and_objects_at_depth_2(self):
        _, p1, _ = _pending("item")
        _, p2, _ = _pending("item", select_depth=2)
        self.assertNotIn("owner", p1[0]["document"])
        self.assertIn("owner { id }", p2[0]["document"])
        # a nested object leaf rides in the whole value; no typed projection is minted for it
        self.assertEqual([p["name"] for p in p2], [p["name"] for p in p1])

    def test_typed_projections_skip_numbers_and_lists_and_narrow_enums_to_strings(self):
        _, pending, notes = _pending("item")
        names = [p["name"] for p in pending]
        self.assertEqual(names, ["item", "itemName", "itemNote", "itemKind", "itemFresh"])
        by = {p["name"]: p for p in pending}
        self.assertEqual(by["itemKind"]["type_ast"]["result"], gi.MAYBE_STRING)
        self.assertEqual(by["itemKind"]["check"]["enum"], ["GADGET", "WIDGET"])
        self.assertEqual(by["itemFresh"]["type_ast"]["result"], gi.MAYBE_BOOL)
        self.assertFalse(by["itemName"]["required"])  # `item` itself is nullable
        self.assertTrue(any("size: Int" in n and "not projected" in n for n in notes))

    def test_optional_argument_is_omitted_unless_the_operator_binds_it(self):
        _, pending, notes = _pending("items")
        self.assertEqual(pending[0]["document"], "query Q { items { name size tags note kind fresh } }")
        self.assertTrue(any("optional argument `limit` omitted" in n for n in notes))
        _, bound, notes = _pending("items", observe={"items": {"limit": "1"}})
        self.assertEqual(bound[0]["document"],
                         "query Q($limit: Int) { items(limit: $limit) { name size tags note kind fresh } }")
        self.assertEqual(bound[0]["args"][1], {"kind": "int", "value": 1})
        self.assertTrue(any("INCLUDED at the operator's binding" in n for n in notes))

    def test_list_result_has_no_typed_projections(self):
        _, pending, _ = _pending("items")
        self.assertEqual(len(pending), 1)
        self.assertIn("list", pending[0]["check"])

    def test_observe_arg_on_an_unknown_parameter_refuses(self):
        st, name, why = _pending("item", observe={"item": {"nope": "x"}})
        self.assertEqual(st, "skip")
        self.assertIn("nope", why)

    def test_walk_refuses_mutations_and_reports(self):
        pending, skipped, notes, report, unheeded = gi.walk(_schema())
        self.assertEqual(report, {"query_fields": 4, "compiled": 4, "refused": 0, "mutation_fields": 1,
                                  "subscription_fields": 0, "projections": 10})
        self.assertEqual([s[0] for s in skipped], ["putItem"])
        self.assertIn("read-only by rule", skipped[0][1])
        self.assertEqual(unheeded, [])
        _, _, _, _, unheeded = gi.walk(_schema(), observe={"ghost": {"x": "1"}})
        self.assertEqual(unheeded, ["ghost"])

    def test_auth_header_rides_as_a_secret_placeholder(self):
        _, pending, _ = _pending("secret", auth_header=("Authorization", "Bearer {{secret:api_token}}"))
        self.assertIn("Bearer {{secret:api_token}}", json.dumps(pending[0]["body_ast"]))


class RefusalBoundaryTest(unittest.TestCase):
    def _mini(self, query_fields, extra_types=()):
        S = lambda n: {"kind": "SCALAR", "name": n, "ofType": None}  # noqa: E731
        types = [{"kind": "OBJECT", "name": "Query", "fields": query_fields, "interfaces": []},
                 {"kind": "SCALAR", "name": "String"}, {"kind": "SCALAR", "name": "Int"},
                 {"kind": "OBJECT", "name": "Empty", "interfaces": [],
                  "fields": [{"name": "sub", "args": [], "type": {"kind": "OBJECT", "name": "Empty", "ofType": None}}]},
                 {"kind": "OBJECT", "name": "Needy", "interfaces": [],
                  "fields": [{"name": "v", "type": S("String"),
                              "args": [{"name": "k", "type": {"kind": "NON_NULL", "name": None, "ofType": S("String")},
                                        "defaultValue": None}]}]},
                 {"kind": "UNION", "name": "Either", "possibleTypes": [{"kind": "OBJECT", "name": "Empty"}]},
                 *extra_types]
        schema = {"queryType": {"name": "Query"}, "mutationType": None, "subscriptionType": None, "types": types}
        return gi.type_index(schema), query_fields

    def test_object_without_argument_free_scalar_leaf_refuses(self):
        tindex, fields = self._mini([{"name": "e", "args": [], "type": {"kind": "OBJECT", "name": "Empty", "ofType": None}},
                                     {"name": "n", "args": [], "type": {"kind": "OBJECT", "name": "Needy", "ofType": None}}])
        for f in fields:
            st, _, why = gi.build_field(tindex, f)
            self.assertEqual(st, "skip", f["name"])
            self.assertIn("no legal selection set", why)

    def test_union_return_refuses(self):
        tindex, fields = self._mini([{"name": "u", "args": [], "type": {"kind": "UNION", "name": "Either", "ofType": None}}])
        st, _, why = gi.build_field(tindex, fields[0])
        self.assertEqual(st, "skip")
        self.assertIn("UNION", why)

    def test_parameter_named_base_collides(self):
        S = {"kind": "SCALAR", "name": "String", "ofType": None}
        tindex, fields = self._mini([{"name": "f", "type": S,
                                      "args": [{"name": "base", "defaultValue": None,
                                                "type": {"kind": "NON_NULL", "name": None, "ofType": S}}]}])
        st, _, why = gi.build_field(tindex, fields[0])
        self.assertEqual(st, "skip")
        self.assertIn("collision", why)

    def test_scalar_root_field_projects_the_value_alone(self):
        S = {"kind": "SCALAR", "name": "String", "ofType": None}
        tindex, fields = self._mini([{"name": "version", "args": [], "type": S}])
        st, pending, _ = gi.build_field(tindex, fields[0])
        self.assertEqual(st, "ok")
        self.assertEqual(len(pending), 1)
        self.assertEqual(pending[0]["document"], "query Q { version }")


class ConformanceTest(unittest.TestCase):
    J = staticmethod(gi._json_to_value)

    def test_nullability_and_lists_and_presence(self):
        chk_nn = {"nonnull": True, "kind": "scalar", "name": "String"}
        self.assertFalse(gi._observed_conforms(self.J(None), chk_nn, "x")[0])
        self.assertTrue(gi._observed_conforms(self.J(None), {**chk_nn, "nonnull": False}, "x")[0])
        lst = {"nonnull": True, "list": chk_nn}
        self.assertTrue(gi._observed_conforms(self.J(["a"]), lst, "x")[0])
        self.assertFalse(gi._observed_conforms(self.J(["a", None]), lst, "x")[0])
        obj = {"nonnull": True, "kind": "object", "fields": {"a": chk_nn}}
        self.assertTrue(gi._observed_conforms(self.J({"a": "1", "extra": 2}), obj, "x")[0])
        ok, why = gi._observed_conforms(self.J({"b": "1"}), obj, "x")
        self.assertFalse(ok)
        self.assertIn("absent from the response data", why)

    def test_scalars_and_enums(self):
        self.assertFalse(gi._observed_conforms(self.J("1"), {"nonnull": True, "kind": "scalar", "name": "Int"}, "x")[0])
        self.assertFalse(gi._observed_conforms(self.J(1.5), {"nonnull": True, "kind": "scalar", "name": "Int"}, "x")[0])
        self.assertTrue(gi._observed_conforms(self.J(1.5), {"nonnull": True, "kind": "scalar", "name": "Float"}, "x")[0])
        self.assertTrue(gi._observed_conforms(self.J({"any": 1}), {"nonnull": True, "kind": "scalar", "name": "Custom"}, "x")[0])
        en = {"nonnull": True, "kind": "scalar", "name": "Kind", "enum": ["A", "B"]}
        self.assertTrue(gi._observed_conforms(self.J("A"), en, "x")[0])
        self.assertFalse(gi._observed_conforms(self.J("C"), en, "x")[0])


class ObserveArgParsingTest(unittest.TestCase):
    def test_first_dot_splits_field_from_argument(self):
        self.assertEqual(gi.parse_observe_args(["item.name=a=b", "items.limit=2"]),
                         {"item": {"name": "a=b"}, "items": {"limit": "2"}})
        with self.assertRaises(SystemExit):
            gi.parse_observe_args(["noequals"])

    def test_values_parse_by_sort(self):
        self.assertEqual(gi._arg_value("json", '{"b": [1, true, null]}')["tag"], "JObj")
        self.assertEqual(gi._arg_value("bool", "true"), {"kind": "bool", "value": True})
        with self.assertRaises(ValueError):
            gi._arg_value("bool", "yes")


@unittest.skipUnless(VALIDATOR.exists(), "nl-validator not built")
class ObservationGateTest(unittest.TestCase):
    """The live half against the in-repo fake service: both transports, the absent name, auth,
    trace sharing between sibling projections, and a violated promise."""

    PORT = 18891

    @classmethod
    def setUpClass(cls):
        cls.base = f"http://127.0.0.1:{cls.PORT}"
        cls.svc = subprocess.Popen([sys.executable, str(FAKE), "--port", str(cls.PORT)],
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

    def _ingest(self, *extra, schema=SCHEMA):
        tmp = tempfile.mkdtemp(prefix="nl-graphql-")
        code = 0
        try:
            gi.main([str(schema), "--out", tmp, "--verify-against", f"{self.base}/graphql", *extra])
        except SystemExit as e:
            code = e.code or 0
        recs = {p.name.replace(".v0.2.json", ""): json.load(open(p)) for p in Path(tmp).glob("*.v0.2.json")}
        return code, tmp, recs

    def _result(self, rec):
        return rec["examples"][0]["result"]

    def test_get_transport_materializes_every_projection_from_one_call_per_document(self):
        code, tmp, recs = self._ingest("--observe-arg", "item.name=gw18-widget", "--auth-bearer", "api_token")
        self.assertEqual(code, 0)
        self.assertEqual(set(recs), {"health", "healthstatus", "item", "itemname", "itemnote", "itemkind",
                                     "itemfresh", "items", "secret", "secretvalue"})
        # sibling projections share the ONE recorded trace (same request, same trc_ address)
        traces = {n: r["examples"][0]["trace"] for n, r in recs.items()}
        self.assertEqual(len({traces[n] for n in ("item", "itemname", "itemnote", "itemkind", "itemfresh")}), 1)
        self.assertEqual(traces["health"], traces["healthstatus"])
        self.assertNotEqual(traces["health"], traces["item"])
        self.assertEqual(self._result(recs["healthstatus"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "string", "value": "ok"}})
        self.assertEqual(self._result(recs["itemkind"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "string", "value": "WIDGET"}})
        self.assertEqual(self._result(recs["itemfresh"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "bool", "value": True}})
        self.assertEqual(self._result(recs["itemnote"]), {"kind": "variant", "tag": "None"})  # null leaf
        self.assertEqual(recs["health"]["signature"]["effects"], ["net.read"])
        self.assertEqual(recs["item"]["examples"][0]["args"][1], {"kind": "string", "value": "gw18-widget"})
        # the trace holds the document in the request target and the placeholder, never the token
        trace = json.load(open(Path(tmp) / "trace-secret-0.json"))
        detail = trace["ops"][0]["detail"]
        self.assertEqual(detail["method"], "GET")
        self.assertIn("?query=query%20Q%20%7B%20secret", detail["url"])
        self.assertIn({"name": "Authorization", "value": "Bearer {{secret:api_token}}"}, detail["headers"])
        self.assertNotIn("test-token", json.dumps(trace))
        # every record replays offline with no secrets
        for name in recs:
            r = subprocess.run([str(VALIDATOR), "run", str(Path(tmp) / f"{name}.v0.2.json"), "--records", tmp],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"{name}: {r.stdout}\n{r.stderr}")

    def test_absent_name_is_a_value_not_a_status(self):
        code, tmp, recs = self._ingest("--observe-arg", "item.name=nope")
        self.assertEqual(code, 1)  # `secret` fails without --auth-bearer; everything else materializes
        self.assertEqual(self._result(recs["item"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "variant", "tag": "JNull"}})
        self.assertEqual(self._result(recs["itemname"]), {"kind": "variant", "tag": "None"})
        self.assertNotIn("secret", recs)
        self.assertNotIn("secretvalue", recs)

    def test_post_transport_materializes_with_a_json_body_and_net_write(self):
        code, tmp, recs = self._ingest("--transport", "post", "--auth-bearer", "api_token")
        self.assertEqual(code, 0)
        self.assertEqual(recs["health"]["signature"]["effects"], ["net.write"])
        detail = json.load(open(Path(tmp) / "trace-health-0.json"))["ops"][0]["detail"]
        self.assertEqual(detail["method"], "POST")
        self.assertEqual(json.loads(detail["body"]), {"query": "query Q { health { status } }"})
        self.assertIn({"name": "Content-Type", "value": "application/json"}, detail["headers"])
        self.assertEqual(self._result(recs["healthstatus"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "string", "value": "ok"}})

    def test_a_promise_the_service_does_not_honor_publishes_nothing(self):
        # Declare `health.status` as Int: the observed "ok" violates the declared type.
        doc = json.load(open(SCHEMA))
        for t in doc["data"]["__schema"]["types"]:
            if t["name"] == "Health":
                t["fields"][0]["type"]["ofType"]["name"] = "Int"
        tmp = tempfile.mkdtemp(prefix="nl-graphql-bad-")
        p = Path(tmp) / "bad.json"
        json.dump(doc, open(p, "w"))
        code, out, recs = self._ingest("--auth-bearer", "api_token", schema=p)
        self.assertEqual(code, 1)
        self.assertNotIn("health", recs)
        self.assertNotIn("healthstatus", recs)  # a sibling inherits the failed observation
        self.assertIn("item", recs) if "item" in recs else None
        self.assertIn("items", recs)

    def test_unheeded_binding_halts_before_any_call(self):
        code, tmp, recs = self._ingest("--observe-arg", "ghost.x=1")
        self.assertEqual(code, 2)
        self.assertEqual(recs, {})


if __name__ == "__main__":
    unittest.main()
