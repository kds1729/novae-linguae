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

Both 400 on an unencoded character in the request target — so a worked example whose query
value contains a space passes ONLY IF the client percent-encoded it (the url_encode gate).

    python3 fake_service.py [--port 8878] [--token test-token]
"""

import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse


class Handler(BaseHTTPRequestHandler):
    store = {}
    token = "test-token"

    def _reply(self, status, body=b""):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/json")
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
        if self.headers.get("X-Api-Key") == self.token:
            return True
        self._reply(401, b'{"error":"unauthorized"}')
        return False

    def do_GET(self):
        # /health is an UNAUTHENTICATED liveness probe (no Bearer token, no params) — the
        # smallest operation, and the one an API-description generator emits with no auth header.
        if self.path == "/health":
            self._reply(200, b'{"status":"ok"}')
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
    args = ap.parse_args()
    Handler.token = args.token
    HTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
