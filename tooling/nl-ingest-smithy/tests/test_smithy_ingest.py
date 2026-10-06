"""Tests for nl-ingest-smithy.

Offline: the model -> plan synthesis (operation discovery through resources, the readonly /
required decision table, bodies, the refusal boundary) needs no service. Live: the observation
gate against the in-repo fake service's `/rpc` (a valid empty call, two description-guaranteed
rejections, an operator-supplied input, the effect opt-in, trace sharing) plus two LYING services
(one that accepts a required-violating input, one that refuses at the transport), each record
certified by the built `nl-validator` and replayed offline.

    /home/claude/sandbox/ft-venv/bin/python -m unittest discover -s tests
"""

import json
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_ADAPTER = _HERE.parent
REPO_ROOT = _ADAPTER.parent.parent
FAKE = REPO_ROOT / "tooling" / "fake-service" / "fake_service.py"
MODEL = _ADAPTER / "examples" / "item-rpc.smithy.json"

sys.path.insert(0, str(_ADAPTER))
sys.path.insert(0, str(REPO_ROOT / "tooling" / "fake-service"))
import smithy_ingest as si  # noqa: E402


def _load(path):
    with open(path) as f:
        return json.load(f)


def _model():
    return si.load_model(str(MODEL))


def _plan(name, **kw):
    model = _model()
    facts = si.service_facts(model)
    return si.plan_operation(model, facts, f"fake#{name}", **kw)


def _names(plan):
    return [p["name"] for p in plan["pending"]]


class ModelTest(unittest.TestCase):
    def test_example_model_is_the_fake_services(self):
        import fake_service
        self.assertEqual(_load(MODEL), fake_service.rpc_model())

    def test_service_facts_come_from_the_service_shape(self):
        facts = si.service_facts(_model())
        self.assertEqual(facts, {"target_prefix": "ItemRpc", "content_type": "application/x-amz-json-1.1",
                                 "short": "itemrpc"})

    def test_sdk_id_names_the_short_form_when_present(self):
        model = _model()
        model["shapes"]["fake#ItemRpc"]["traits"]["aws.api#service"] = {"sdkId": "Global Accelerator"}
        self.assertEqual(si.service_facts(model)["short"], "global-accelerator")

    def test_rest_json_service_is_refused_with_the_route(self):
        model = _load(MODEL)
        svc = model["shapes"]["fake#ItemRpc"]
        svc["traits"] = {"aws.protocols#restJson1": {}}
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(model, f)
        with self.assertRaises(SystemExit) as cm:
            si.load_model(f.name)
        self.assertIn("restJson1", str(cm.exception))
        self.assertIn("nl-ingest-openapi", str(cm.exception))

    def test_operations_are_collected_through_resources(self):
        model = _model()
        shapes = model["shapes"]
        svc = shapes["fake#ItemRpc"]
        # rebind: two operations directly, two through a resource (one lifecycle, one child-resource)
        svc["operations"] = [{"target": "fake#ListItems"}, {"target": "fake#ResetAll"}]
        svc["resources"] = [{"target": "fake#Item"}]
        shapes["fake#Item"] = {"type": "resource", "read": {"target": "fake#GetItem"},
                               "resources": [{"target": "fake#ItemNote"}]}
        shapes["fake#ItemNote"] = {"type": "resource", "operations": [{"target": "fake#PutItem"}]}
        self.assertEqual(si.collect_operations(model),
                         ["fake#GetItem", "fake#ListItems", "fake#PutItem", "fake#ResetAll"])


class PlanTest(unittest.TestCase):
    def test_readonly_without_required_observes_the_empty_input_and_licenses_projections(self):
        st, plan, notes = _plan("ListItems")
        self.assertEqual(st, "ok")
        self.assertEqual((plan["mode"], plan["expect"], plan["input"]), ("empty", "2xx", {}))
        self.assertTrue(plan["readonly"])
        self.assertEqual(_names(plan), ["ListItems", "ListItemsOutput", "ListItemsItems", "ListItemsRegion",
                                        "ListItemsEmpty"])
        kinds = {p["name"]: p["type_ast"]["result"] for p in plan["pending"]}
        self.assertEqual(kinds["ListItems"], si.INT)
        self.assertEqual(kinds["ListItemsOutput"], si.MAYBE_JSON)
        self.assertEqual(kinds["ListItemsItems"], si.MAYBE_JSON)  # a list rides as Json
        self.assertEqual(kinds["ListItemsRegion"], si.MAYBE_STRING)
        self.assertEqual(kinds["ListItemsEmpty"], si.MAYBE_BOOL)
        self.assertTrue(any("`count` (long) not projected" in n for n in notes))
        self.assertTrue(any("`updatedAt` (timestamp) not projected" in n for n in notes))
        self.assertEqual(plan["pending"][0]["intent"], ["io", "io/network/http", "query/lookup",
                                                        "query/lookup/itemrpc-list-items"])
        self.assertEqual(plan["pending"][1]["intent"][-1], "parse/itemrpc-list-items-output")

    def test_every_record_takes_base_and_one_json_input(self):
        st, plan, _ = _plan("ListItems")
        for p in plan["pending"]:
            self.assertEqual(p["type_ast"]["params"], [si.STRING, si.JSON_T])
            self.assertEqual([q["name"] for q in p["body_ast"]["params"]], ["base", "input"])

    def test_required_member_means_the_empty_input_is_observed_as_a_rejection(self):
        for op in ("GetItem", "PutItem"):
            st, plan, notes = _plan(op)
            self.assertEqual(st, "ok", op)
            self.assertEqual((plan["mode"], plan["expect"]), ("reject", "reject"))
            self.assertEqual(_names(plan), [op])  # a rejection licenses no output projection
            self.assertTrue(any("violates required member(s)" in n for n in notes))
        st, plan, _ = _plan("PutItem")
        self.assertFalse(plan["readonly"])
        self.assertEqual(plan["pending"][0]["intent"], ["io", "io/network/http", "io/network/http/itemrpc-put-item"])
        self.assertEqual(plan["required"], ["body", "name"])

    def test_mutating_without_required_has_no_effect_free_call_and_refuses(self):
        st, name, why = _plan("ResetAll")
        self.assertEqual((st, name), ("skip", "ResetAll"))
        self.assertIn("no effect-free observation exists", why)
        self.assertIn("--observe-effect", why)

    def test_operator_readonly_declaration_licenses_observing_an_unmarked_operation(self):
        st, plan, notes = _plan("ResetAll", readonly_globs=["Reset*"])
        self.assertEqual(st, "ok")
        self.assertEqual((plan["mode"], plan["expect"]), ("empty", "2xx"))
        self.assertTrue(any("OPERATOR DECLARATION" in n for n in notes))
        self.assertEqual(_names(plan), ["ResetAll", "ResetAllOutput"])  # `cleared` is numeric: noted
        st, plan, notes = _plan("ListItems", readonly_globs=["List*"])
        self.assertFalse(any("OPERATOR DECLARATION" in n for n in notes))  # trait already says so

    def test_observe_arg_needs_a_readonly_operation_with_its_required_members(self):
        st, plan, _ = _plan("GetItem", observe={"GetItem": '{"name": "rpc-widget"}'})
        self.assertEqual((st, plan["mode"], plan["expect"]), ("ok", "arg", "2xx"))
        self.assertEqual(plan["input"], {"name": "rpc-widget"})
        self.assertEqual(_names(plan), ["GetItem", "GetItemOutput", "GetItemName", "GetItemBody", "GetItemKind",
                                        "GetItemOwner"])  # `version` (integer) noted, not projected
        st, _, why = _plan("GetItem", observe={"GetItem": '{"nam": "x"}'})
        self.assertEqual(st, "skip")
        self.assertIn("lacks required member(s) ['name']", why)
        st, _, why = _plan("PutItem", observe={"PutItem": '{"name": "a", "body": "b"}'})
        self.assertEqual(st, "skip")
        self.assertIn("MUTATING", why)
        self.assertIn("--observe-effect", why)
        st, _, why = _plan("GetItem", observe={"GetItem": "not json"})
        self.assertEqual(st, "skip")
        self.assertIn("not JSON", why)

    def test_observe_effect_is_the_mutating_opt_in_only(self):
        st, plan, notes = _plan("ResetAll", observe_effect={"ResetAll": "{}"})
        self.assertEqual((st, plan["mode"], plan["expect"]), ("ok", "effect", "2xx"))
        self.assertTrue(any("EFFECT OPT-IN" in n for n in notes))
        self.assertEqual(_names(plan), ["ResetAll"])  # a mutating observation licenses no projection
        st, _, why = _plan("ListItems", observe_effect={"ListItems": "{}"})
        self.assertEqual(st, "skip")
        self.assertIn("readonly", why)
        st, _, why = _plan("PutItem", observe_effect={"PutItem": '{"name": "a"}'})
        self.assertEqual(st, "skip")
        self.assertIn("lacks required member(s) ['body']", why)
        st, _, why = _plan("PutItem", observe={"PutItem": "{}"}, observe_effect={"PutItem": "{}"})
        self.assertEqual(st, "skip")
        self.assertIn("BOTH", why)

    def test_body_is_one_post_with_the_target_header_and_the_rendered_input(self):
        st, plan, _ = _plan("GetItem")
        body = json.dumps(plan["pending"][0]["body_ast"])
        self.assertIn('"value": "POST"', body)
        self.assertIn('"value": "ItemRpc.GetItem"', body)
        self.assertIn('"value": "X-Amz-Target"', body)
        self.assertIn('"value": "application/x-amz-json-1.1"', body)
        self.assertIn('"name": "render_json"', body)
        self.assertNotIn("url_encode", body)
        self.assertNotIn("Authorization", body)  # identity is the signing boundary's, not the record's

    def test_walk_reports_and_flags_unheeded_bindings(self):
        facts, plans, skipped, notes, report, unheeded = si.walk(_model(), observe={"Nope": "{}"})
        self.assertEqual(report, {"operations": 4, "planned": 3, "refused": 1, "readonly": 2, "projections": 7})
        self.assertEqual([s[0] for s in skipped], ["ResetAll"])
        self.assertEqual(unheeded, ["Nope"])

    def test_binding_grammar(self):
        self.assertEqual(si.parse_bindings(['GetItem.input={"name":"a"}'], "--observe-arg"),
                         {"GetItem": '{"name":"a"}'})
        for bad in ("GetItem={}", "GetItem.name=x", "GetItem.input", ".input={}"):
            with self.assertRaises(SystemExit):
                si.parse_bindings([bad], "--observe-arg")


class ConformanceTest(unittest.TestCase):
    def _check(self, doc, target="fake#GetItemResponse"):
        return si.observed_conforms(_model()["shapes"], target, doc, "output")

    def test_declared_types_are_held(self):
        self.assertEqual(self._check({"name": "a", "body": "b", "version": 1, "kind": "WIDGET", "owner": {"id": "u"}}),
                         (True, ""))
        self.assertFalse(self._check({"name": 3})[0])
        self.assertFalse(self._check({"version": "1"})[0])
        self.assertFalse(self._check({"kind": "BLOB"})[0])
        self.assertFalse(self._check({"owner": "u1"})[0])
        self.assertFalse(self._check({"owner": {"id": 7}})[0])
        self.assertFalse(self._check({"items": "x"}, "fake#ListItemsResponse")[0])
        self.assertFalse(self._check({"items": [1]}, "fake#ListItemsResponse")[0])
        self.assertFalse(self._check({"empty": "no"}, "fake#ListItemsResponse")[0])
        self.assertEqual(self._check({"items": [], "updatedAt": "2026-10-06T00:00:00Z"}, "fake#ListItemsResponse"),
                         (True, ""))  # timestamp wire form is trait-chosen: unconstrained

    def test_null_reads_as_absent_unless_required(self):
        self.assertEqual(self._check({"name": None, "owner": None}), (True, ""))
        model = _model()
        model["shapes"]["fake#GetItemResponse"]["members"]["name"]["traits"] = {"smithy.api#required": {}}
        ok, why = si.observed_conforms(model["shapes"], "fake#GetItemResponse", {"name": None}, "output")
        self.assertFalse(ok)
        self.assertIn("is null", why)
        ok, why = si.observed_conforms(model["shapes"], "fake#GetItemResponse", {"body": "x"}, "output")
        self.assertFalse(ok)
        self.assertIn("absent", why)


class _LyingHandler(BaseHTTPRequestHandler):
    """Two descriptions that do not hold: a service that ACCEPTS a required-violating input
    (status 200 to everything), and one that refuses at the transport (403, no error document)."""
    status = 200
    body = b'{"name":"made"}'

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        self.send_response(self.status)
        self.send_header("Content-Type", "application/json" if self.body.startswith(b"{") else "text/plain")
        self.send_header("Content-Length", str(len(self.body)))
        self.end_headers()
        self.wfile.write(self.body)

    def log_message(self, fmt, *args):
        pass


class ObservationGateTest(unittest.TestCase):
    """The live half against the in-repo fake service's /rpc."""

    PORT = 18893

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

    def _rpc(self, op, inp):
        req = urllib.request.Request(f"{self.base}/rpc", data=json.dumps(inp).encode(), method="POST",
                                     headers={"X-Amz-Target": f"ItemRpc.{op}",
                                              "Content-Type": "application/x-amz-json-1.1"})
        with urllib.request.urlopen(req) as r:
            return json.loads(r.read())

    def _ingest(self, *extra, endpoint=None):
        tmp = tempfile.mkdtemp(prefix="nl-smithy-")
        code = 0
        out = []
        import io
        import contextlib
        buf = io.StringIO()
        try:
            with contextlib.redirect_stdout(buf):
                si.main([str(MODEL), "--out", tmp, "--verify-against", endpoint or f"{self.base}/rpc", *extra])
        except SystemExit as e:
            code = e.code or 0
        out = buf.getvalue()
        recs = {p.name.replace(".v0.2.json", ""): _load(p) for p in Path(tmp).glob("*.v0.2.json")}
        return code, tmp, recs, out

    def _result(self, rec):
        return rec["examples"][0]["result"]

    def test_default_run_observes_one_valid_call_and_two_rejections_without_changing_state(self):
        before = self._rpc("ListItems", {})
        code, tmp, recs, out = self._ingest()
        after = self._rpc("ListItems", {})
        self.assertEqual(code, 0, out)
        self.assertEqual(before, after)  # PutItem {} was observed and changed NOTHING
        self.assertEqual(set(recs), {"getitem", "listitems", "listitemsoutput", "listitemsitems", "listitemsregion",
                                     "listitemsempty", "putitem"})
        self.assertEqual(self._result(recs["getitem"]), {"kind": "int", "value": 400})
        self.assertEqual(self._result(recs["putitem"]), {"kind": "int", "value": 400})
        self.assertEqual(self._result(recs["listitems"]), {"kind": "int", "value": 200})
        self.assertEqual(self._result(recs["listitemsregion"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "string", "value": "fake-1"}})
        self.assertEqual(self._result(recs["listitemsempty"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "bool", "value": False}})
        self.assertEqual(self._result(recs["listitemsitems"])["tag"], "Just")
        # sibling projections share ONE trace; the rejections have their own
        traces = {n: r["examples"][0]["trace"] for n, r in recs.items()}
        self.assertEqual(len({traces[n] for n in ("listitems", "listitemsoutput", "listitemsitems",
                                                   "listitemsregion", "listitemsempty")}), 1)
        self.assertNotEqual(traces["getitem"], traces["putitem"])
        for r in recs.values():
            self.assertEqual(r["signature"]["effects"], ["net.write"])
            self.assertEqual(r["examples"][0]["args"][0], {"kind": "string", "value": f"{self.base}/rpc"})
            self.assertEqual(r["examples"][0]["args"][1], {"kind": "variant", "tag": "JObj",
                                                           "payload": {"kind": "map", "entries": []}})
        self.assertIn("GetItem: observation-gate=OK(400 ValidationException)", out)
        self.assertIn("all certified + replayed", out)
        # the trace carries the wire: the target header and the rendered empty input
        trace = _load(Path(tmp) / "trace-putitem-0.json")
        detail = json.dumps(trace["ops"][0])
        self.assertIn("ItemRpc.PutItem", detail)
        self.assertIn("ValidationException", detail)

    def test_observe_arg_materializes_the_real_document_and_its_typed_projections(self):
        code, tmp, recs, out = self._ingest("--observe-arg", 'GetItem.input={"name":"rpc-widget"}')
        self.assertEqual(code, 0, out)
        self.assertEqual(self._result(recs["getitem"]), {"kind": "int", "value": 200})
        self.assertEqual(self._result(recs["getitemkind"]),
                         {"kind": "variant", "tag": "Just", "payload": {"kind": "string", "value": "WIDGET"}})
        self.assertEqual(self._result(recs["getitemowner"])["tag"], "Just")
        self.assertEqual(self._result(recs["getitemoutput"])["tag"], "Just")
        self.assertEqual(recs["getitem"]["examples"][0]["args"][1]["payload"]["entries"],
                         [{"key": "name", "value": {"kind": "variant", "tag": "JStr",
                                                    "payload": {"kind": "string", "value": "rpc-widget"}}}])
        self.assertNotIn("getitemversion", recs)  # integer: noted, never projected

    def test_observe_effect_performs_the_mutation_once_and_records_it(self):
        self._rpc("PutItem", {"name": "doomed", "body": "x"})
        code, tmp, recs, out = self._ingest("--observe-effect", "ResetAll.input={}")
        self.assertEqual(code, 0, out)
        self.assertEqual(self._result(recs["resetall"]), {"kind": "int", "value": 200})
        self.assertNotIn("doomed", self._rpc("ListItems", {})["items"])  # the effect happened, once
        self.assertEqual(recs["resetall"]["intent_tags"], ["io", "io/network/http", "io/network/http/itemrpc-reset-all"])
        trace = _load(Path(tmp) / "trace-resetall-0.json")
        self.assertIn("cleared", json.dumps(trace["ops"][0]))

    def test_readonly_declaration_observes_an_unmarked_operation_at_the_empty_input(self):
        code, tmp, recs, out = self._ingest("--readonly", "Reset*")
        self.assertEqual(code, 0, out)
        self.assertIn("resetalloutput", recs)
        self.assertEqual(recs["resetall"]["intent_tags"][2], "query/lookup")
        self.assertIn("readonly by OPERATOR DECLARATION", out)

    def test_records_certify_and_replay_offline_by_hand(self):
        code, tmp, recs, out = self._ingest()
        for name in ("listitemsregion", "putitem"):
            r = subprocess.run([si._VALIDATOR, "run", str(Path(tmp) / f"{name}.v0.2.json"), "--records", tmp],
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def _lying(self, status, body):
        _LyingHandler.status, _LyingHandler.body = status, body
        srv = HTTPServer(("127.0.0.1", 0), _LyingHandler)
        t = threading.Thread(target=srv.serve_forever, daemon=True)
        t.start()
        try:
            return self._ingest(endpoint=f"http://127.0.0.1:{srv.server_address[1]}/rpc")
        finally:
            srv.shutdown()
            srv.server_close()

    def test_a_service_that_accepts_a_required_violating_input_fails_the_gate_loudly(self):
        code, tmp, recs, out = self._lying(200, b'{"name":"made"}')
        self.assertEqual(code, 1)
        self.assertNotIn("putitem", recs)
        self.assertNotIn("getitem", recs)
        self.assertIn("PutItem: observation-gate=FAIL the service answered 200 to `{}`", out)
        self.assertIn("an effect MAY HAVE OCCURRED", out)
        self.assertIn("GetItem: observation-gate=FAIL", out)
        self.assertNotIn("GetItem: observation-gate=FAIL the service answered 200 to `{}` although member(s) "
                         "['name'] are required — the description's contract does not hold — a MUTATING", out)
        # the valid empty call still observed fine against a 200 (its document conforms: nothing declared is wrong)
        self.assertIn("listitems", recs)

    def test_an_access_denial_is_not_the_models_rejection(self):
        # measured live: an out-of-boundary AWS call answers 400 AccessDeniedException — the same
        # status a validation rejection carries, but the input was never looked at
        code, tmp, recs, out = self._lying(400, b'{"__type":"AccessDeniedException","message":"no"}')
        self.assertEqual(code, 1)
        self.assertEqual(recs, {})
        self.assertIn("PutItem: observation-gate=FAIL the service answered 400 AccessDeniedException — the REQUEST "
                      "was refused", out)
        self.assertIn("GetItem: observation-gate=FAIL the service answered 400 AccessDeniedException", out)

    def test_a_transport_refusal_is_not_an_observation(self):
        code, tmp, recs, out = self._lying(403, b"Forbidden")
        self.assertEqual(code, 1)
        self.assertEqual(recs, {})
        self.assertIn("PutItem: observation-gate=FAIL the service answered 403 without an awsJson error document", out)
        self.assertIn("ListItems: observation-gate=FAIL the service answered 403, not 200", out)


class CliTest(unittest.TestCase):
    def test_offline_run_is_a_licensing_report_that_writes_nothing(self):
        tmp = tempfile.mkdtemp(prefix="nl-smithy-off-")
        r = subprocess.run([sys.executable, str(_ADAPTER / "smithy_ingest.py"), str(MODEL), "--out", tmp],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("licensed ListItemsRegion : (string, Json) -> Maybe string  [net.write]  observe `{}` -> 200", r.stdout)
        self.assertIn("licensed PutItem : (string, Json) -> int  [net.write]  observe `{}` -> rejection", r.stdout)
        self.assertIn("skip ResetAll", r.stdout)
        self.assertIn("0 materialized", r.stdout)
        self.assertEqual(list(Path(tmp).iterdir()), [])

    def test_bindings_need_a_service_and_a_declared_operation(self):
        tmp = tempfile.mkdtemp(prefix="nl-smithy-off-")
        r = subprocess.run([sys.executable, str(_ADAPTER / "smithy_ingest.py"), str(MODEL), "--out", tmp,
                            "--observe-arg", "GetItem.input={}"], capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("require --verify-against", r.stderr)
        r = subprocess.run([sys.executable, str(_ADAPTER / "smithy_ingest.py"), str(MODEL), "--out", tmp,
                            "--verify-against", "http://127.0.0.1:1/rpc", "--observe-arg", "Nope.input={}"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("refuse: bindings name operation(s) ['Nope']", r.stdout)
        self.assertEqual(list(Path(tmp).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
