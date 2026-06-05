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
_PREFIX_KIND = {"fn": "function-record", "msg": "message", "expr": "body", "type": "type"}

# (kind, schema_version) -> schema filename in COMMONS_SPEC_DIR.
_SCHEMA = {
    ("function-record", "0.1.0"): "function-record.schema.json",
    ("function-record", "0.2.0"): "function-record.v0.2.schema.json",
    ("message", "0.1.0"): "message.schema.json",
    ("message", "0.2.0"): "message.v0.2.schema.json",
    ("body", "0.1.0"): "body-expression.schema.json",
    ("type", "0.1.0"): "type-expression.schema.json",
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


def verify_record(raw):
    """Verify a parsed record. Returns (kind, schema_version) on success, else raises VerifyError."""
    if not isinstance(raw, dict):
        raise VerifyError("schema_invalid", "record must be a JSON object")
    if not isinstance(raw.get("hash"), str):
        raise VerifyError("schema_invalid", "record is missing a string 'hash'")

    kind, version = detect(raw)
    if kind is None:
        raise VerifyError("unsupported_kind", f"unknown address prefix in {raw.get('hash')!r}")
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
    return kind, version


def extract(raw, kind):
    """Pull the queryable columns out of a verified record (function records carry the signature)."""
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
    }
