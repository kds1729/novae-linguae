#!/usr/bin/env python3
"""A reference fake HTTP service for effectful exit gates (stdlib-only, deterministic).

A minimal authenticated key-value resource API — the smallest service against which a
create -> verify -> delete workflow (spec/expressiveness.md GW6) can run end to end without
touching any real, billable, or mutable-in-the-world system:

    PUT    /items/{name}   store the request body under {name}; 201 if new, 200 if replaced
    GET    /items/{name}   200 + the stored body, or 404
    DELETE /items/{name}   204 if it existed, 404 otherwise

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

    python3 fake_service.py [--port 8878] [--token test-token] [--oauth-client id:secret]
"""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse


class Handler(BaseHTTPRequestHandler):
    store = {}
    things = {}
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

    def do_POST(self):
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

    def do_GET(self):
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
