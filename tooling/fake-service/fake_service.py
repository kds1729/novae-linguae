#!/usr/bin/env python3
"""A reference fake HTTP service for effectful exit gates (stdlib-only, deterministic).

A minimal authenticated key-value resource API — the smallest service against which a
create -> verify -> delete workflow (spec/expressiveness.md GW6) can run end to end without
touching any real, billable, or mutable-in-the-world system:

    PUT    /items/{name}   store the request body under {name}; 201 if new, 200 if replaced
    GET    /items/{name}   200 + the stored body, or 404
    DELETE /items/{name}   204 if it existed, 404 otherwise
    PUT    /items/{name}/note   store a note ON an item — 404 unless the item exists (the
                                world-state dependency surface, spec/world-state.md)

Every /items request must carry `Authorization: Bearer <token>` (the --token argument), else
401 — which is what makes the gate exercise the secret-placeholder path ({{secret:...}} header
values) rather than skipping auth. Names are CLIENT-chosen, so there is no server-assigned
nondeterminism and a run replays byte-identically. State is in-memory only.

The GW10 surface (spec/expressiveness.md — query params, header params, apiKey auth) uses a
SECOND auth style, `X-Api-Key: <token>`, on two read endpoints:

    GET /search?q=<term>&limit=<n>   200 if q non-empty and limit an integer, else 400
    GET /version                     200 if an X-Client-Id header is present, else 400
    POST /upload                     X-Api-Key-authed multipart/form-data: 201 iff the body
                                     parses against the Content-Type boundary and carries the
                                     required parts (archive, note), else 400 — the exit gate
                                     for the adapter's compiled multipart forms

Both 400 on an unencoded character in the request target — so a worked example whose query
value contains a space passes ONLY IF the client percent-encoded it (the url_encode gate).

The GW13 surface adds a THIRD auth style, OAuth2 client-credentials:

    POST /token             form grant_type=client_credentials + the fixed client id/secret
                            (--oauth-client, default gw13-client:gw13-secret) -> 200
                            {"access_token": "<derived token>"}; anything else 400/401
    GET  /reports/summary   200 + a FIXED JSON body iff `Authorization: Bearer <that token>`

The issued token is deliberately DISTINCT from --token, so a passing gate proves the client
really exchanged credentials at /token rather than replaying the static bearer secret.

The GW14 surface (spec/expressiveness.md — response headers) is the piece the client-chosen
/items API deliberately avoided: SERVER-assigned identity, delivered in a response header.

    POST   /things          Bearer-authed; stores the body under an id the SERVER derives
                            (th_<sha256(body)[:12]> — server-assigned yet deterministic, so
                            runs still replay byte-identically); always 201 + `Location:
                            /things/{id}` (idempotent create-or-replace)
    GET    /things/{id}     200 + the stored body, or 404 (Bearer-authed)
    DELETE /things/{id}     204 if it existed, 404 otherwise (Bearer-authed)
    GET    /latest          307 + `Location: /health` — unauthenticated; the one-hop redirect
                            a bounded in-language follower resolves

A client that drops response headers cannot find a POSTed thing at all — the documented 200
on the follow-up GET proves the Location header was read.

The GW15 surface (pagination — a zero-pull: no new builtin, the Link header is DATA):

    GET /list?page=N        three fixed pages (N in 1..3), unauthenticated; pages 1-2 carry an
                            RFC 8288 `Link` header whose rel="next" names the following page —
                            page 2's Link ALSO carries rel="prev" first, so a client must parse
                            the header, not substring-match it; page 3 has prev but NO next, so
                            a page-walk stops by absence, not by hitting a depth bound

A client that cannot read the Link header sees one page and no way to the rest.

The GraphQL surface (tooling/nl-ingest-graphql — the second description-layer adapter) is ONE
endpoint serving a fixed schema, on both transports GraphQL-over-HTTP allows:

    GET  /graphql?query=…&variables=…   the document in the request target (percent-encoded)
    POST /graphql                       {"query": …, "variables": …} as a JSON body

    An introspection document (`__schema`) answers the fixed schema; anything else resolves
    against fixed data: `health { status }`, `item(name: ID!)` (null when absent — GraphQL
    spells absence as a value, the transport status stays 200), `items(limit: Int)` (a list),
    `secret { value }` (401 unless `Authorization: Bearer <token>` — the ONE field that needs
    the secret-placeholder path; introspection declares no auth), and a `Mutation` root
    (`putItem`) an ingestion adapter must refuse. An unknown root field answers 200 with
    `errors` and no `data` — the GraphQL validation-failure shape.

The JSON-RPC surface (tooling/nl-ingest-smithy — the third description-layer adapter) is the
AWS `awsJson1_x` wire shape: ONE endpoint, every operation a POST, the operation named by a
header, and the HTTP status carrying no verb semantics at all:

    POST /rpc               `X-Amz-Target: ItemRpc.<Operation>` + `Content-Type:
                            application/x-amz-json-1.1` + a JSON body (the operation's input)

    Operations (the fixed Smithy model `rpc_model()`): `ListItems` (readonly, no required
    member) -> 200 {items, count, region, empty}; `GetItem` (readonly, `name` required) -> 200
    the item, or an `ItemNotFoundException` (404 by @httpError); `PutItem` (mutating, `name`
    + `body` required) -> 200; `ResetAll` (mutating, NO required member — the operation with
    no effect-free call) -> 200 {cleared}. A body violating the model's `required` members is
    REJECTED with `ValidationException` (400) BEFORE any effect — the awsJson validation
    contract the adapter's observation gate relies on. An unknown target answers
    `UnknownOperationException`; a non-JSON body `SerializationException`.

    python3 fake_service.py [--port 8878] [--token test-token] [--oauth-client id:secret]
"""

import argparse
import hashlib
import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse


# ---- GraphQL: the fixed schema, as an introspection result, and its resolvers ----------------

def _gt(kind, name=None, of=None):
    return {"kind": kind, "name": name, "ofType": of}


def _nn(t):
    return _gt("NON_NULL", None, t)


def _lst(t):
    return _gt("LIST", None, t)


def _gfield(name, typ, args=()):
    return {"name": name, "description": None,
            "args": [{"name": a, "description": None, "type": t, "defaultValue": None} for a, t in args],
            "type": typ, "isDeprecated": False, "deprecationReason": None}


def _gtype(kind, name, fields=None, enum_values=None):
    return {"kind": kind, "name": name, "description": None, "fields": fields, "inputFields": None,
            "interfaces": [] if kind == "OBJECT" else None,
            "enumValues": ([{"name": v, "description": None, "isDeprecated": False, "deprecationReason": None}
                            for v in enum_values] if enum_values is not None else None),
            "possibleTypes": None}


def gql_introspection():
    """The fixed schema in the shape the standard introspection query returns (`__schema`)."""
    S = lambda n: _gt("SCALAR", n)  # noqa: E731
    O = lambda n: _gt("OBJECT", n)  # noqa: E731
    return {
        "queryType": {"name": "Query"}, "mutationType": {"name": "Mutation"}, "subscriptionType": None,
        "types": [
            _gtype("OBJECT", "Query", [
                _gfield("health", _nn(O("Health"))),
                _gfield("item", O("Item"), [("name", _nn(S("ID")))]),
                _gfield("items", _nn(_lst(_nn(O("Item")))), [("limit", S("Int"))]),
                _gfield("secret", O("Secret")),
            ]),
            _gtype("OBJECT", "Mutation", [
                _gfield("putItem", _nn(O("Item")), [("name", _nn(S("ID"))), ("size", _nn(S("Int")))]),
            ]),
            _gtype("OBJECT", "Health", [_gfield("status", _nn(S("String")))]),
            _gtype("OBJECT", "Item", [
                _gfield("name", _nn(S("ID"))), _gfield("size", _nn(S("Int"))),
                _gfield("tags", _nn(_lst(_nn(S("String"))))), _gfield("note", S("String")),
                _gfield("kind", _nn(_gt("ENUM", "Kind"))), _gfield("fresh", _nn(S("Boolean"))),
                _gfield("owner", O("Owner")),
            ]),
            _gtype("OBJECT", "Owner", [_gfield("id", _nn(S("ID")))]),
            _gtype("OBJECT", "Secret", [_gfield("value", _nn(S("String")))]),
            _gtype("ENUM", "Kind", enum_values=["WIDGET", "GADGET"]),
            _gtype("SCALAR", "String"), _gtype("SCALAR", "ID"), _gtype("SCALAR", "Int"),
            _gtype("SCALAR", "Boolean"), _gtype("SCALAR", "Float"),
        ],
        "directives": [],
    }


_GQL_ITEMS = {
    "gw18-widget": {"name": "gw18-widget", "size": 3, "tags": ["blue", "small"], "note": None,
                    "kind": "WIDGET", "fresh": True, "owner": {"id": "u1"}},
    "gw18-gadget": {"name": "gw18-gadget", "size": 7, "tags": [], "note": "spare",
                    "kind": "GADGET", "fresh": False, "owner": None},
}


# ---- JSON-RPC (awsJson1_1): the fixed Smithy model and its dispatcher --------------------------

_RPC_NS = "fake"
RPC_SERVICE = "ItemRpc"


def _sm_struct(members, required=()):
    return {"type": "structure",
            "members": {m: ({"target": t, "traits": {"smithy.api#required": {}}} if m in required
                            else {"target": t}) for m, t in members.items()}}


def _sm_error(status=None):
    traits = {"smithy.api#error": "client"}
    if status is not None:
        traits["smithy.api#httpError"] = status
    return {"type": "structure", "members": {"message": {"target": "smithy.api#String"}}, "traits": traits}


def rpc_model():
    """The fixed service as a Smithy JSON AST (the file format the AWS service models ship in):
    four operations, two of them `readonly`, one mutating operation with NO required member."""
    ns = _RPC_NS
    op = lambda inp, out, errors=(), readonly=False: {  # noqa: E731
        "type": "operation", "input": {"target": f"{ns}#{inp}"}, "output": {"target": f"{ns}#{out}"},
        "errors": [{"target": f"{ns}#{e}"} for e in ("ValidationException",) + tuple(errors)],
        **({"traits": {"smithy.api#readonly": {}}} if readonly else {}),
    }
    return {
        "smithy": "2.0",
        "shapes": {
            f"{ns}#{RPC_SERVICE}": {
                "type": "service", "version": "2026-10-06",
                "operations": [{"target": f"{ns}#{o}"} for o in ("ListItems", "GetItem", "PutItem", "ResetAll")],
                "traits": {"aws.protocols#awsJson1_1": {}},
            },
            f"{ns}#ListItems": op("ListItemsRequest", "ListItemsResponse", readonly=True),
            f"{ns}#GetItem": op("GetItemRequest", "GetItemResponse", ("ItemNotFoundException",), readonly=True),
            f"{ns}#PutItem": op("PutItemRequest", "PutItemResponse"),
            f"{ns}#ResetAll": op("ResetAllRequest", "ResetAllResponse"),
            f"{ns}#ListItemsRequest": _sm_struct({"prefix": "smithy.api#String"}),
            f"{ns}#ListItemsResponse": _sm_struct({"items": f"{ns}#StringList", "count": "smithy.api#Long",
                                                   "region": "smithy.api#String", "empty": "smithy.api#Boolean",
                                                   "updatedAt": "smithy.api#Timestamp"}),
            f"{ns}#GetItemRequest": _sm_struct({"name": "smithy.api#String"}, required=("name",)),
            f"{ns}#GetItemResponse": _sm_struct({"name": "smithy.api#String", "body": "smithy.api#String",
                                                 "version": "smithy.api#Integer", "kind": f"{ns}#Kind",
                                                 "owner": f"{ns}#Owner"}),
            f"{ns}#PutItemRequest": _sm_struct({"name": "smithy.api#String", "body": "smithy.api#String"},
                                               required=("name", "body")),
            f"{ns}#PutItemResponse": _sm_struct({"name": "smithy.api#String"}),
            f"{ns}#ResetAllRequest": _sm_struct({}),
            f"{ns}#ResetAllResponse": _sm_struct({"cleared": "smithy.api#Integer"}),
            f"{ns}#StringList": {"type": "list", "member": {"target": "smithy.api#String"}},
            f"{ns}#Kind": {"type": "enum", "members": {"WIDGET": {"target": "smithy.api#Unit",
                                                                   "traits": {"smithy.api#enumValue": "WIDGET"}},
                                                        "GADGET": {"target": "smithy.api#Unit",
                                                                   "traits": {"smithy.api#enumValue": "GADGET"}}}},
            f"{ns}#Owner": _sm_struct({"id": "smithy.api#String"}),
            f"{ns}#ValidationException": _sm_error(),
            f"{ns}#ItemNotFoundException": _sm_error(404),
        },
    }


_RPC_REQUIRED = {"ListItems": (), "GetItem": ("name",), "PutItem": ("name", "body"), "ResetAll": ()}
_RPC_FIXED_ITEMS = {"rpc-widget": {"name": "rpc-widget", "body": "blue", "version": 1, "kind": "WIDGET",
                                   "owner": {"id": "u1"}}}


class Handler(BaseHTTPRequestHandler):
    store = {}
    things = {}
    notes = {}
    rpc_items = {k: dict(v) for k, v in _RPC_FIXED_ITEMS.items()}
    token = "test-token"
    api_key = "test-token"  # the X-Api-Key credential; main() defaults it to --token
    oauth_client = ("gw13-client", "gw13-secret")

    @property
    def oauth_token(self):
        return f"{self.token}-oauth"

    def _reply(self, status, body=b"", headers=()):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/json")
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def _authed(self):
        if self.headers.get("Authorization") == f"Bearer {self.token}":
            return True
        self._reply(401, b'{"error":"unauthorized"}')
        return False

    def _name(self):
        if self.path.startswith("/items/") and len(self.path) > len("/items/"):
            return self.path[len("/items/"):]
        self._reply(404, b'{"error":"not found"}')
        return None

    def _rpc_error(self, status, typ, message):
        self._reply(status, json.dumps({"__type": typ, "message": message}).encode(),
                    headers=[("x-amzn-ErrorType", typ)])

    def _rpc(self):
        """awsJson1_1: the operation is the `X-Amz-Target` header's suffix, the body its input.
        Validation (required members) happens BEFORE any effect — a rejected call changes
        nothing, which is what lets an ingestion adapter observe a mutating operation safely."""
        target = self.headers.get("X-Amz-Target") or ""
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        try:
            inp = json.loads(raw or b"{}")
        except ValueError:
            self._rpc_error(400, "SerializationException", "body is not JSON")
            return
        prefix, _, op = target.rpartition(".")
        if prefix != RPC_SERVICE or op not in _RPC_REQUIRED:
            self._rpc_error(400, "UnknownOperationException", f"unknown target {target!r}")
            return
        if not isinstance(inp, dict):
            self._rpc_error(400, "SerializationException", "input is not an object")
            return
        missing = [m for m in _RPC_REQUIRED[op] if m not in inp]
        if missing:
            self._rpc_error(400, "ValidationException", f"missing required member(s): {', '.join(missing)}")
            return
        items = self.rpc_items
        if op == "ListItems":
            names = sorted(n for n in items if n.startswith(inp.get("prefix") or ""))
            self._reply(200, json.dumps({"items": names, "count": len(names), "region": "fake-1",
                                         "empty": not names, "updatedAt": 1759708800}).encode())
        elif op == "GetItem":
            it = items.get(inp["name"])
            if it is None:
                self._rpc_error(404, "ItemNotFoundException", f"no item {inp['name']!r}")
            else:
                self._reply(200, json.dumps(it).encode())
        elif op == "PutItem":
            items[inp["name"]] = {"name": inp["name"], "body": inp["body"], "version": 1}
            self._reply(200, json.dumps({"name": inp["name"]}).encode())
        else:  # ResetAll: the effect no input can make effect-free
            n = len(items)
            items.clear()
            items.update({k: dict(v) for k, v in _RPC_FIXED_ITEMS.items()})
            self._reply(200, json.dumps({"cleared": n}).encode())

    def do_POST(self):
        if self.path == "/rpc":
            self._rpc()
            return
        if self.path == "/graphql":
            length = int(self.headers.get("Content-Length") or 0)
            try:
                doc = json.loads(self.rfile.read(length) or b"{}")
            except ValueError:
                self._reply(400, b'{"errors":[{"message":"body is not JSON"}]}')
                return
            self._graphql(str(doc.get("query") or ""), doc.get("variables") or {})
            return
        # GW14: server-assigned identity. The id is DERIVED from the body (sha256 prefix), so
        # it is genuinely server-chosen (the client cannot name it) yet deterministic — a rerun
        # replays byte-identically. Always 201 + Location (idempotent create-or-replace).
        if self.path == "/things":
            if not self._authed():
                return
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length)
            thing_id = "th_" + hashlib.sha256(body).hexdigest()[:12]
            self.things[thing_id] = body
            self._reply(201, body, headers=[("Location", f"/things/{thing_id}")])
            return
        # Multipart exit gate: a compiled form must really parse — boundary from the
        # Content-Type, per-part names from Content-Disposition, framing delimiters intact.
        if self.path == "/upload":
            if not self._api_keyed():
                return
            ctype = self.headers.get("Content-Type") or ""
            boundary = ""
            for piece in ctype.split(";"):
                piece = piece.strip()
                if piece.startswith("boundary="):
                    boundary = piece[len("boundary="):].strip('"')
            if not ctype.startswith("multipart/") or not boundary:
                self._reply(400, b'{"error":"not multipart"}')
                return
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length).decode("utf-8", "replace")
            delim = "--" + boundary
            if not raw.startswith(delim) or delim + "--" not in raw:
                self._reply(400, b'{"error":"bad framing"}')
                return
            names = []
            for part in raw.split(delim)[1:]:
                head = part.split("\r\n\r\n", 1)[0]
                for line in head.split("\r\n"):
                    if line.lower().startswith("content-disposition:") and 'name="' in line:
                        names.append(line.split('name="', 1)[1].split('"', 1)[0])
            if {"archive", "note"} <= set(names):
                self._reply(201, ('{"received":' + str(len(names)) + "}").encode())
            else:
                self._reply(400, b'{"error":"missing required parts"}')
            return
        # GW13: the OAuth2 client-credentials token endpoint. Form-encoded per RFC 6749 §4.4.
        if self.path != "/token":
            self._reply(404, b'{"error":"not found"}')
            return
        length = int(self.headers.get("Content-Length") or 0)
        form = parse_qs(self.rfile.read(length).decode("utf-8", "replace"))
        if form.get("grant_type", [""])[0] != "client_credentials":
            self._reply(400, b'{"error":"unsupported_grant_type"}')
            return
        cid, csec = self.oauth_client
        if form.get("client_id", [""])[0] != cid or form.get("client_secret", [""])[0] != csec:
            self._reply(401, b'{"error":"invalid_client"}')
            return
        body = ('{"access_token":"' + self.oauth_token + '","token_type":"Bearer"}').encode()
        self._reply(200, body)

    def do_PUT(self):
        if not self._authed():
            return
        # World-state dependency surface (spec/world-state.md — the requires-a-VPC/guarantees-a-
        # subnet shape at fake-service scale): a NOTE lives ON an item, so storing one REQUIRES
        # the item to exist (404 otherwise) and leaves the note existing. The smallest operation
        # with a genuine world-state precondition — what `check-plan` discharges symbolically
        # before any effect, and what a live run confirms the declaration truthful about.
        if self.path.startswith("/items/") and self.path.endswith("/note"):
            name = self.path[len("/items/"):-len("/note")]
            if not name:
                self._reply(404, b'{"error":"not found"}')
                return
            if name not in self.store:
                self._reply(404, b'{"error":"no such item"}')
                return
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length)
            existed = name in self.notes
            self.notes[name] = body
            self._reply(200 if existed else 201, body)
            return
        name = self._name()
        if name is None:
            return
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length)
        existed = name in self.store
        self.store[name] = body
        self._reply(200 if existed else 201, body)

    def _api_keyed(self):
        # A separate credential from the Bearer token (defaults equal for back-compat): a client
        # that binds credentials per scheme passes both surfaces in one run; one that reuses a
        # single value across schemes fails whichever surface got the wrong one.
        if self.headers.get("X-Api-Key") == self.api_key:
            return True
        self._reply(401, b'{"error":"unauthorized"}')
        return False

    def _graphql(self, query, variables):
        """Resolve a document against the fixed schema. No parser: the root field is the first
        name after the first `{` (the adapter's documents are `query Q(...) { field(...) {...} }`),
        which is all the fixed resolvers need."""
        if "__schema" in query:
            self._reply(200, json.dumps({"data": {"__schema": gql_introspection()}}).encode())
            return
        m = re.search(r"\{\s*(\w+)", query)
        root = m.group(1) if m else ""
        if root == "health":
            data = {"health": {"status": "ok"}}
        elif root == "item":
            data = {"item": _GQL_ITEMS.get(str((variables or {}).get("name")))}
        elif root == "items":
            data = {"items": [_GQL_ITEMS[k] for k in sorted(_GQL_ITEMS)]}
        elif root == "secret":
            if not self._authed():
                return
            data = {"secret": {"value": "gw18-secret-value"}}
        else:
            self._reply(200, json.dumps({"errors": [{"message": f"Cannot query field \"{root}\" on type "
                                                                f"\"Query\"."}]}).encode())
            return
        self._reply(200, json.dumps({"data": data}).encode())

    def do_GET(self):
        if self.path.startswith("/graphql"):
            qs = parse_qs(urlparse(self.path).query)
            variables = qs.get("variables", ["{}"])[0]
            try:
                variables = json.loads(variables)
            except ValueError:
                self._reply(400, b'{"errors":[{"message":"variables is not JSON"}]}')
                return
            self._graphql(qs.get("query", [""])[0], variables)
            return
        # /health is an UNAUTHENTICATED liveness probe (no Bearer token, no params) — the
        # smallest operation, and the one an API-description generator emits with no auth header.
        if self.path == "/health":
            self._reply(200, b'{"status":"ok"}')
            return
        # GW14: the one-hop redirect an in-language bounded follower resolves. 307 preserves
        # the method; the target is the unauthenticated liveness probe.
        if self.path == "/latest":
            self._reply(307, b"", headers=[("Location", "/health")])
            return
        # GW15: the paginated collection. Three fixed pages; the rel="next" Link is the ONLY
        # route to the following page (a client that drops headers sees one page). Page 2's
        # Link carries rel="prev" BEFORE rel="next", so the header must be parsed, and page 3
        # has no next, so a page-walk terminates by absence.
        if self.path == "/list" or self.path.startswith("/list?"):
            qs = parse_qs(urlparse(self.path).query)
            try:
                page = int(qs.get("page", ["1"])[0])
            except ValueError:
                page = 0
            bodies = {1: b'{"items":["a1","a2"]}', 2: b'{"items":["b1"]}', 3: b'{"items":["c1","c2","c3"]}'}
            if page not in bodies:
                self._reply(404, b'{"error":"no such page"}')
                return
            links = []
            if page > 1:
                links.append(f'</list?page={page - 1}>; rel="prev"')
            if page < 3:
                links.append(f'</list?page={page + 1}>; rel="next"')
            headers = [("Link", ", ".join(links))] if links else []
            self._reply(200, bodies[page], headers=headers)
            return
        if self.path.startswith("/things/"):
            if not self._authed():
                return
            body = self.things.get(self.path[len("/things/"):])
            if body is None:
                self._reply(404, b'{"error":"no such thing"}')
            else:
                self._reply(200, body)
            return
        if self.path == "/reports/summary":
            # GW13: protected by the /token-ISSUED bearer only — the static --token is refused
            # here, so a 200 proves a real client-credentials exchange happened.
            if self.headers.get("Authorization") != f"Bearer {self.oauth_token}":
                self._reply(401, b'{"error":"unauthorized"}')
                return
            self._reply(200, b'{"status":"green","total":12}')
            return
        parsed = urlparse(self.path)
        if parsed.path in ("/search", "/version"):
            # The GW10 surface, X-Api-Key-authed. An unencoded space never even reaches here
            # (a malformed request line), but any other raw non-ASCII byte in the target is a
            # deterministic 400 — so the documented 200 PROVES the client percent-encoded.
            if any(ord(c) > 126 or c == " " for c in self.path):
                self._reply(400, b'{"error":"unencoded character in request target"}')
                return
            if not self._api_keyed():
                return
            if parsed.path == "/version":
                if not self.headers.get("X-Client-Id"):
                    self._reply(400, b'{"error":"missing X-Client-Id header"}')
                    return
                self._reply(200, b'{"version":"1.0.0"}')
                return
            qs = parse_qs(parsed.query)
            q = qs.get("q", [""])[0]
            limit = qs.get("limit", [""])[0]
            if not q or not limit.isdigit():
                self._reply(400, b'{"error":"q (non-empty) and limit (integer) are required"}')
                return
            names = sorted(n for n in self.store if q in n)[: int(limit)]
            self._reply(200, ('{"results":' + str(names).replace("'", '"') + "}").encode())
            return
        if not self._authed():
            return
        name = self._name()
        if name is None:
            return
        if name in self.store:
            self._reply(200, self.store[name])
        else:
            self._reply(404, b'{"error":"no such item"}')

    def do_DELETE(self):
        if not self._authed():
            return
        if self.path.startswith("/things/"):
            thing_id = self.path[len("/things/"):]
            if thing_id in self.things:
                del self.things[thing_id]
                self._reply(204)
            else:
                self._reply(404, b'{"error":"no such thing"}')
            return
        name = self._name()
        if name is None:
            return
        if name in self.store:
            del self.store[name]
            self._reply(204)
        else:
            self._reply(404, b'{"error":"no such item"}')

    def log_message(self, fmt, *args):  # quiet: the gate reads statuses, not logs
        pass


def main():
    ap = argparse.ArgumentParser(description="Reference fake HTTP service for effectful exit gates.")
    ap.add_argument("--port", type=int, default=8878)
    ap.add_argument("--token", default="test-token")
    ap.add_argument("--api-key", default=None,
                    help="the X-Api-Key value the keyed endpoints accept (default: same as --token, "
                         "the historical single-credential behavior; set it differently to exercise "
                         "a client's PER-SCHEME credential binding)")
    ap.add_argument("--oauth-client", default="gw13-client:gw13-secret",
                    help="the client-credentials pair /token accepts, as id:secret")
    args = ap.parse_args()
    Handler.token = args.token
    Handler.api_key = args.api_key if args.api_key is not None else args.token
    Handler.oauth_client = tuple(args.oauth_client.split(":", 1))
    HTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
