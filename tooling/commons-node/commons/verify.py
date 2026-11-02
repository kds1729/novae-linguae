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
    "trc": "trace", "plan": "plan",
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
    ("plan", "0.1.0"): "plan.schema.json",
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


def _unparse(command, ast):
    """The validator's canonical surface rendering of one AST (`unparse-type` / `unparse-predicate`),
    or None if it cannot render it — rendering is a convenience for readers, never an admission check."""
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump(ast, f)
        path = f.name
    try:
        r = _run(command, path)
        if r.returncode != 0:
            return None
        return (r.stdout or "").strip() or None
    except VerifyError:
        return None
    finally:
        os.unlink(path)


def _fn_addresses(node, out):
    """Every content address a predicate applies (`{"kind": "app", "op": "fn_…"}`), in order."""
    if isinstance(node, dict):
        op = node.get("op")
        if node.get("kind") == "app" and isinstance(op, str) and op.startswith("fn_") and op not in out:
            out.append(op)
        for v in node.values():
            _fn_addresses(v, out)
    elif isinstance(node, list):
        for v in node:
            _fn_addresses(v, out)
    return out


def surface(raw, kind):
    """The decision fields of a function record in canonical surface syntax (2026-11-02): the type,
    the contract (every refinement, labelled by its kind) and the laws (labelled by name), plus the
    content addresses they apply. Measured need: an agent choosing among a search shortlist saw only
    names and a JSON type blob — `allocate`'s `post: fn_…(result) == t` (the parts sum to the total),
    which is what separates it from a plain `divide`, was not in the summary at all. A string
    (v0.1) expression is kept as written; an AST the validator cannot render is skipped."""
    if kind != "function-record":
        return {}
    sig = raw.get("signature") or {}
    out = {}
    t = sig.get("type")
    if isinstance(t, str):
        out["type"] = t
    elif t is not None:
        rendered = _unparse("unparse-type", t)
        if rendered:
            out["type"] = rendered
    addresses = []
    contract = []
    for r in sig.get("refinements") or []:
        e = r.get("expr") if isinstance(r, dict) else None
        text = e if isinstance(e, str) else (_unparse("unparse-predicate", e) if e is not None else None)
        if text:
            contract.append(f"{r.get('kind', 'refinement')}: {text}")
            _fn_addresses(e, addresses)
    laws = []
    for p in raw.get("properties") or []:
        e = p.get("expr") if isinstance(p, dict) else None
        text = e if isinstance(e, str) else (_unparse("unparse-predicate", e) if e is not None else None)
        if text:
            laws.append(f"{p.get('name', 'law')}: {text}")
            _fn_addresses(e, addresses)
    if contract:
        out["contract"] = contract
    if laws:
        out["laws"] = laws
    if addresses:
        out["addresses"] = addresses
    return out


def extract(raw, kind):
    """Pull the queryable columns out of a verified record (function records carry the signature;
    certifications and eval attestations carry the `subject` they attest to)."""
    signature = raw.get("signature", {}) if kind == "function-record" else {}
    type_value = signature.get("type")
    return {
        "surface": surface(raw, kind),
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
