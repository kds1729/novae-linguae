"""Verification on ingest — the *only* admission gate (spec/commons.md).

Reuses the Rust reference validator (`nl-validator`) so this node agrees byte-for-byte with the
canonical form, hashing, schema validation, and signature rules. A record is admitted iff:

  1. its content-address recomputes correctly (hash, and signature for messages) — `nl-validator verify`;
  2. it validates against the schema named by its (kind, schema_version) — `nl-validator validate`.

This is mechanical, not editorial: nothing here decides a record is unwelcome on grounds other than
"it does not verify" (principle 7). A future optimization can port these checks in-process (nl_core
for hashing, jsonschema for schema, PyNaCl for Ed25519) as long as it agrees with the validator.
"""

import json
import os
import subprocess
import tempfile

from django.conf import settings

# Address prefix -> artifact kind.
_PREFIX_KIND = {
    "fn": "function-record", "msg": "message", "expr": "body", "type": "type",
    "cert": "certification", "wgt": "weights", "evl": "eval-attestation",
    "trc": "trace",
}

# (kind, schema_version) -> schema filename in COMMONS_SPEC_DIR.
_SCHEMA = {
    ("function-record", "0.1.0"): "function-record.schema.json",
    ("function-record", "0.2.0"): "function-record.v0.2.schema.json",
    ("message", "0.1.0"): "message.schema.json",
    ("message", "0.2.0"): "message.v0.2.schema.json",
    ("body", "0.1.0"): "body-expression.schema.json",
    ("type", "0.1.0"): "type-expression.schema.json",
    ("certification", "0.2.0"): "certification.schema.json",
    ("weights", "0.1.0"): "weights.schema.json",
    ("eval-attestation", "0.1.0"): "eval-attestation.schema.json",
    ("trace", "0.1.0"): "trace.schema.json",
}


class VerifyError(Exception):
    def __init__(self, code, detail=""):
        super().__init__(code)
        self.code = code
        self.detail = detail


def _prefix(address):
    return address.split("_", 1)[0] if isinstance(address, str) and "_" in address else ""


def detect(raw):
    """Return (kind, schema_version) from the record's declared address + schema_version."""
    return _PREFIX_KIND.get(_prefix(raw.get("hash"))), raw.get("schema_version")


def _schema_path(kind, version):
    name = _SCHEMA.get((kind, version))
    return os.path.join(settings.COMMONS_SPEC_DIR, name) if name else None


def _run(*args):
    try:
        return subprocess.run([settings.COMMONS_VALIDATOR, *args], capture_output=True, text=True)
    except FileNotFoundError as exc:
        raise VerifyError("verifier_unavailable", str(exc))


# Body-expression node kinds (body-expression.schema.json). A BARE body has no embedded `hash`
# field — the whole expression IS the hashed content — so it is self-addressing on ingest.
# `variant`/`tuple` are the construction forms a 0-argument body can top out at (the same omission
# was fixed in the Rust validator's two body-kind lists).
_BODY_EXPR_KINDS = {"var", "lit", "app", "let", "lambda", "case", "field", "variant", "tuple"}


def verify_record(raw):
    """Verify a parsed record. Returns (kind, schema_version, address) on success, else raises
    VerifyError. For hash-carrying artifacts the address is the embedded `hash` (verified by
    recomputation); a bare body expression is validated against the body schema and its `expr_…`
    address computed server-side."""
    if not isinstance(raw, dict):
        raise VerifyError("schema_invalid", "record must be a JSON object")
    if not isinstance(raw.get("hash"), str):
        if raw.get("kind") in _BODY_EXPR_KINDS:
            return _verify_bare_body(raw)
        if raw.get("kind") == "trace":
            return _verify_bare_trace(raw)
        raise VerifyError("schema_invalid", "record is missing a string 'hash'")

    kind, version = detect(raw)
    if kind is None:
        raise VerifyError("unsupported_kind", f"unknown address prefix in {raw.get('hash')!r}")
    if kind == "type":
        # A type artifact is the bare type-expression AST plus its `hash` (no schema_version —
        # the expression grammar admits no extra fields). Validate the STRIPPED expression, then
        # recompute the address with the explicit kind (type-node kinds collide with body kinds,
        # so the validator never auto-detects types), plus the well-formedness pass the schema
        # itself delegates (var scoping, rank-1, unique fields/tags).
        return _verify_type(raw)
    schema = _schema_path(kind, version)
    if schema is None:
        raise VerifyError("unsupported_kind", f"no schema for {kind} {version!r}")

    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(raw, f)
        path = f.name
    try:
        validated = _run("validate", schema, path)
        if validated.returncode != 0:
            raise VerifyError("schema_invalid", (validated.stderr or "").strip())
        verified = _run("verify", path)
        if verified.returncode != 0:
            err = (verified.stderr or "").lower()
            code = "signature_invalid" if "signature" in err else "hash_mismatch"
            raise VerifyError(code, (verified.stderr or "").strip())
    finally:
        os.unlink(path)
    return kind, version, raw["hash"]


def _verify_type(raw):
    """Verify a `type_…` artifact: the bare type expression (hash stripped) must validate against
    the type-expression schema AND pass the well-formedness check, and the declared address must
    recompute under the explicit type kind."""
    expr = {k: v for k, v in raw.items() if k != "hash"}
    schema = _schema_path("type", "0.1.0")
    if schema is None:
        raise VerifyError("unsupported_kind", "no schema for type 0.1.0")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(expr, f)
        path = f.name
    try:
        validated = _run("validate", schema, path)
        if validated.returncode != 0:
            raise VerifyError("schema_invalid", (validated.stderr or "").strip())
        wellformed = _run("check-type", path)
        if wellformed.returncode != 0:
            raise VerifyError("schema_invalid",
                              (wellformed.stdout or wellformed.stderr or "").strip())
        hashed = _run("hash", path, "--kind", "type")
        if hashed.returncode != 0:
            raise VerifyError("schema_invalid", (hashed.stderr or "").strip())
        address = (hashed.stdout or "").strip()
        if address != raw["hash"]:
            raise VerifyError("hash_mismatch",
                              f"declared {raw['hash']!r} but the expression hashes to {address!r}")
    finally:
        os.unlink(path)
    return "type", "0.1.0", address


def _verify_bare_body(raw):
    """Verify a bare body expression: schema-valid, then self-addressing — the node computes the
    `expr_…` content address (there is no embedded hash to check; the content IS the address)."""
    schema = _schema_path("body", "0.1.0")
    if schema is None:
        raise VerifyError("unsupported_kind", "no schema for body 0.1.0")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(raw, f)
        path = f.name
    try:
        validated = _run("validate", schema, path)
        if validated.returncode != 0:
            raise VerifyError("schema_invalid", (validated.stderr or "").strip())
        hashed = _run("hash", path)
        if hashed.returncode != 0:
            raise VerifyError("schema_invalid", (hashed.stderr or "").strip())
        address = (hashed.stdout or "").strip()
        if not address.startswith("expr_"):
            raise VerifyError("schema_invalid", f"body hashed to unexpected address {address!r}")
    finally:
        os.unlink(path)
    return "body", "0.1.0", address


def _verify_bare_trace(raw):
    """Verify a recorded effect trace (spec/trace.schema.json): schema-valid, then self-addressing —
    the node computes the `trc_…` content address exactly as it does for a bare body. Traces are
    unsigned by design: they are content-addressed evidence referenced by a *signed* `observed`
    assert, and replay-verification is what gives them meaning."""
    schema = _schema_path("trace", "0.1.0")
    if schema is None:
        raise VerifyError("unsupported_kind", "no schema for trace 0.1.0")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(raw, f)
        path = f.name
    try:
        validated = _run("validate", schema, path)
        if validated.returncode != 0:
            raise VerifyError("schema_invalid", (validated.stderr or "").strip())
        hashed = _run("hash", path)
        if hashed.returncode != 0:
            raise VerifyError("schema_invalid", (hashed.stderr or "").strip())
        address = (hashed.stdout or "").strip()
        if not address.startswith("trc_"):
            raise VerifyError("schema_invalid", f"trace hashed to unexpected address {address!r}")
    finally:
        os.unlink(path)
    return "trace", "0.1.0", address


def extract(raw, kind):
    """Pull the queryable columns out of a verified record (function records carry the signature;
    certifications and eval attestations carry the `subject` they attest to)."""
    signature = raw.get("signature", {}) if kind == "function-record" else {}
    type_value = signature.get("type")
    return {
        "effects": signature.get("effects", []) or [],
        "capabilities": signature.get("capabilities", []) or [],
        "intent_tags": raw.get("intent_tags", []) or [],
        "name_hints": raw.get("name_hints", []) or [],
        "terminates": signature.get("terminates"),
        "complexity": signature.get("complexity"),
        "type_str": type_value if isinstance(type_value, str)
        else (json.dumps(type_value) if type_value is not None else None),
        "body_hash": raw.get("body_hash"),
        # Certifications: the `fn_…` this certification is about, and its verdict. Eval attestations:
        # the `wgt_…` weights record they attest. Indexed so "attestations about this artifact" is a
        # keyed lookup (views.certifications / views.attestations).
        "subject": raw.get("subject") if kind in ("certification", "eval-attestation") else None,
        "certified": raw.get("certified") if kind == "certification" else None,
    }
