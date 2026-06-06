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

import io
import json
import sys
import urllib.request
from pathlib import Path

DESCRIPTOR_VERSION = "nlb-bootstrap/1"


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


def _http_get(url, timeout=30):
    with urllib.request.urlopen(url, timeout=timeout) as r:   # supports https:// and file://
        return r.read()


def resolve(urls, trusted_dids=None, fetch=_http_get):
    """Try each URL until one yields a usable descriptor. With `trusted_dids`, a descriptor must be
    validly signed by a trusted producer; without it, any descriptor is accepted (status reported).
    Returns (descriptor, status, producer, source_url). Raises BootstrapError if none work."""
    errors = []
    for url in urls:
        try:
            doc = json.loads(fetch(url))
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


def pull_bundle(descriptor, fetch=_http_get):
    """Fetch the descriptor's latest_bundle (trying each url), checking the fetched bundle's digest
    matches the (signed) descriptor's hash. Returns (manifest, records). Raises BootstrapError."""
    from .bundle import BundleError, read_bundle

    lb = descriptor.get("latest_bundle") or {}
    urls = lb.get("urls") or []
    if not urls:
        raise BootstrapError("descriptor has no latest_bundle.urls")
    errors = []
    for url in urls:
        try:
            manifest, records = read_bundle(io.BytesIO(fetch(url)))
        except (BundleError, Exception) as exc:
            errors.append((url, str(exc)))
            continue
        if lb.get("hash") and manifest.get("bundle_digest") != lb["hash"]:
            errors.append((url, "bundle_digest does not match the descriptor's hash"))
            continue
        return manifest, records
    raise BootstrapError(f"could not fetch a bundle matching the descriptor: {errors}")
