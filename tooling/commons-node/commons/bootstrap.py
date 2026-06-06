"""Censorship-resistant bootstrap (spec/resilience.md).

When a node cannot reach Arca or any known peer, it needs to discover *where the data is*: a small,
signed **bootstrap descriptor** that points at live peers and the latest seed bundle. The descriptor
is published to a "dead-drop" channel and fetched trust-but-verify.

First channel: a signed descriptor fetched over **HTTPS** from one or more well-known URLs. The
resolver is pluggable — Nostr / IPNS / a blockchain anchor slot in behind the same interface by
supplying a different `fetch`/URL scheme. Because every record (and bundle) is content-addressed, what
a stranded node fetches *next* is verified by hash regardless; the descriptor signature only attests
*who* published the pointers.

    descriptor = {
      "v": "nlb-bootstrap/1",
      "peers": ["https://node.example.org", ...],
      "latest_bundle": {"hash": "blake2b:…", "urls": ["https://…/commons.nlb", ...]},   # optional
      "producer": "did:nova:…",   # set when signed
      "signature": "ed25519:…",   # advisory provenance (reuses the manifest signer)
    }
"""

import base64
import io
import json
import os
import socket
import ssl
import sys
import urllib.request
from pathlib import Path
from urllib.parse import parse_qs, urlparse

DESCRIPTOR_VERSION = "nlb-bootstrap/1"
DEFAULT_IPFS_GATEWAY = "https://ipfs.io"
DEFAULT_DOH = "https://cloudflare-dns.com/dns-query"
_MAX_REDIRECT = 4


class BootstrapError(Exception):
    pass


def _crypto():
    """Lazily load the shared signer (tooling/crypto-python/nl_crypto.py)."""
    tool = Path(__file__).resolve().parents[2]            # .../tooling
    for p in (str(tool / "crypto-python"), str(tool / "ingest-common")):
        if p not in sys.path:
            sys.path.insert(0, p)
    import nl_crypto
    return nl_crypto


def build_descriptor(peers, latest_bundle=None, sign_seed=None):
    """Build a bootstrap descriptor. `latest_bundle` is {"hash", "urls": [...]} or None. If
    `sign_seed` is given, the descriptor is signed (producer did:nova + signature)."""
    doc = {"v": DESCRIPTOR_VERSION, "peers": list(peers)}
    if latest_bundle:
        doc["latest_bundle"] = latest_bundle
    if sign_seed:
        doc = _crypto().sign_manifest(doc, sign_seed)     # generic dict signer (producer + signature)
    return doc


def verify_descriptor(doc, trusted_dids=None):
    """Return (status, producer). status is 'unsigned' | 'valid' | 'invalid', or 'untrusted' when a
    trust list is supplied and the (valid) signer is not on it."""
    status, producer = _crypto().verify_manifest(doc)
    if trusted_dids is not None and not (status == "valid" and producer in set(trusted_dids)):
        return ("untrusted", producer)
    return (status, producer)


# ---------------------------------------------------------------------------
# Channels. Each fetcher maps a URL to bytes; `_dispatch` picks one by scheme, so a descriptor (or
# bundle) URL list can MIX channels and fall back across them — blocking one channel doesn't sever
# bootstrap. Adding a channel = adding a fetcher to CHANNELS. The scheme dispatch and response parsing
# are what the tests pin; the live transports are reference implementations. Whatever bytes a channel
# returns are still verified by the caller (descriptor signature / bundle hash), so a channel is pure
# untrusted transport.
# ---------------------------------------------------------------------------

def _http_get(url, timeout=30):
    """The single network primitive the HTTP-family channels share (so tests stub one place).
    Supports http(s):// and file://."""
    req = urllib.request.Request(url, headers={"user-agent": "nl-bootstrap/1"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def _fetch_http(url, depth):
    return _http_get(url)


def _fetch_ipns(url, depth):
    """ipns://<name>[?gateway=https://gw] -> GET <gateway>/ipns/<name> (content from an IPFS gateway)."""
    u = urlparse(url)
    name = (u.netloc + u.path).strip("/")
    gateway = parse_qs(u.query).get("gateway", [DEFAULT_IPFS_GATEWAY])[0].rstrip("/")
    return _http_get(f"{gateway}/ipns/{name}")


def _fetch_dns(url, depth):
    """dns://<name>[?doh=https://endpoint] -> DoH TXT query; the (reassembled) TXT value is the
    base64 of the descriptor JSON. DNS-over-HTTPS is itself hard to block and needs no DNS resolver."""
    u = urlparse(url)
    name = (u.netloc + u.path).strip("/")
    doh = parse_qs(u.query).get("doh", [DEFAULT_DOH])[0]
    answers = json.loads(_http_get(f"{doh}?name={name}&type=TXT")).get("Answer", [])
    txt = "".join(a.get("data", "") for a in answers if a.get("type") in (16, None))
    return base64.b64decode(txt.replace('"', "").replace("\\", "").replace(" ", ""))


def _fetch_chain(url, depth):
    """chain://<https-read-endpoint>[#json.path] -> read an on-chain ANCHOR: a small pointer (a
    URL/CID someone wrote to OP_RETURN/calldata and exposes via this read API), then follow it on
    whatever channel it names. The chain holds only the pointer, never the bytes."""
    endpoint, _, path = url[len("chain://"):].partition("#")
    body = _http_get(endpoint)
    pointer = body.decode("utf-8").strip()
    if path:
        node = json.loads(body)
        for key in path.split("."):
            node = node[int(key)] if isinstance(node, list) else node[key]
        pointer = str(node).strip()
    return _dispatch(pointer, depth + 1)


def _fetch_nostr(url, depth):
    """nostr://<relay-host>/<author-hex>[?kind=N] -> the newest matching event's content (the
    descriptor). Nostr's own signature is not checked here; the descriptor inside carries our did:nova
    signature, which is what `--trust` verifies."""
    u = urlparse(url)
    kind = int(parse_qs(u.query).get("kind", ["30078"])[0])
    return _nostr_newest_content(u.netloc, u.path.strip("/"), kind).encode("utf-8")


CHANNELS = {
    "http": _fetch_http, "https": _fetch_http, "file": _fetch_http,
    "ipns": _fetch_ipns, "dns": _fetch_dns, "chain": _fetch_chain, "nostr": _fetch_nostr,
}


def _dispatch(url, depth=0):
    if depth > _MAX_REDIRECT:
        raise BootstrapError("too many channel redirects")
    scheme = urlparse(url).scheme or "file"
    fetch = CHANNELS.get(scheme)
    if fetch is None:
        raise BootstrapError(f"no bootstrap channel for scheme {scheme!r} ({url!r})")
    return fetch(url, depth)


# --- Nostr: a minimal read-only WebSocket client (RFC 6455 framing over stdlib socket+ssl). ----------

def _ws_connect(host, port=443, timeout=15):
    raw = socket.create_connection((host, port), timeout=timeout)
    sock = ssl.create_default_context().wrap_socket(raw, server_hostname=host)
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    sock.sendall((f"GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                  f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            raise BootstrapError("nostr relay closed during handshake")
        buf += chunk
    if b" 101 " not in buf.split(b"\r\n", 1)[0]:
        raise BootstrapError("nostr relay did not upgrade to websocket")
    return sock


def _ws_send_text(sock, text):
    payload = text.encode("utf-8")
    mask, n = os.urandom(4), len(payload)
    header = bytearray([0x81])                                   # FIN + text opcode
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += n.to_bytes(2, "big")
    else:
        header.append(0x80 | 127)
        header += n.to_bytes(8, "big")
    header += mask
    sock.sendall(bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))


def _ws_read_text(sock):
    def readn(n):
        b = b""
        while len(b) < n:
            chunk = sock.recv(n - len(b))
            if not chunk:
                raise BootstrapError("nostr relay closed")
            b += chunk
        return b

    data = b""
    while True:
        h = readn(2)
        fin, opcode, ln = h[0] & 0x80, h[0] & 0x0F, h[1] & 0x7F
        if ln == 126:
            ln = int.from_bytes(readn(2), "big")
        elif ln == 127:
            ln = int.from_bytes(readn(8), "big")
        payload = readn(ln) if ln else b""
        if opcode == 0x8:
            raise BootstrapError("nostr relay sent a close frame")
        if opcode in (0x0, 0x1):                                 # continuation / text
            data += payload
            if fin:
                return data.decode("utf-8")
        # opcode 0x9/0xA (ping/pong) ignored


def _nostr_newest_content(relay_netloc, author, kind, timeout=15, connect=_ws_connect):
    host, _, port = relay_netloc.partition(":")
    sock = connect(host, int(port) if port else 443, timeout=timeout)
    try:
        _ws_send_text(sock, json.dumps(["REQ", "nlb", {"authors": [author], "kinds": [kind], "limit": 1}]))
        newest = None
        for _ in range(200):                                     # bound the read loop
            msg = json.loads(_ws_read_text(sock))
            if msg[0] == "EVENT" and msg[1] == "nlb":
                ev = msg[2]
                if newest is None or ev.get("created_at", 0) > newest.get("created_at", -1):
                    newest = ev
            elif msg[0] == "EOSE":
                break
        if newest is None:
            raise BootstrapError("no matching nostr event found")
        return newest["content"]
    finally:
        sock.close()


def resolve(urls, trusted_dids=None, fetch=None):
    """Try each URL until one yields a usable descriptor (channels chosen by scheme; mixed lists fall
    back across channels). With `trusted_dids`, a descriptor must be validly signed by a trusted
    producer; without it, any descriptor is accepted (status reported). Returns
    (descriptor, status, producer, source_url). Raises BootstrapError if none work."""
    get = fetch or (lambda u: _dispatch(u))
    errors = []
    for url in urls:
        try:
            doc = json.loads(get(url))
        except Exception as exc:
            errors.append((url, f"fetch/parse: {exc}"))
            continue
        if not isinstance(doc, dict) or doc.get("v") != DESCRIPTOR_VERSION:
            errors.append((url, "not an nlb-bootstrap/1 descriptor"))
            continue
        status, producer = verify_descriptor(doc, trusted_dids)
        if trusted_dids is not None and status != "valid":
            errors.append((url, f"signature {status} (producer={producer})"))
            continue
        return doc, status, producer, url
    raise BootstrapError(f"no usable bootstrap descriptor from {list(urls)}: {errors}")


def pull_bundle(descriptor, fetch=None):
    """Fetch the descriptor's latest_bundle (trying each url over its channel), checking the fetched
    bundle's digest matches the (signed) descriptor's hash. Returns (manifest, records)."""
    from .bundle import BundleError, read_bundle

    get = fetch or (lambda u: _dispatch(u))
    lb = descriptor.get("latest_bundle") or {}
    urls = lb.get("urls") or []
    if not urls:
        raise BootstrapError("descriptor has no latest_bundle.urls")
    errors = []
    for url in urls:
        try:
            manifest, records = read_bundle(io.BytesIO(get(url)))
        except (BundleError, Exception) as exc:
            errors.append((url, str(exc)))
            continue
        if lb.get("hash") and manifest.get("bundle_digest") != lb["hash"]:
            errors.append((url, "bundle_digest does not match the descriptor's hash"))
            continue
        return manifest, records
    raise BootstrapError(f"could not fetch a bundle matching the descriptor: {errors}")
