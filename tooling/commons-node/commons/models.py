"""The commons record model.

One row per content-addressed artifact. The integer primary key (`id`) is the monotonic insertion
sequence that drives the `sync` replication cursor. `raw` is the exact record; the remaining columns
are values *extracted* from it on ingest so typed `query` does not have to re-parse JSON.

This is one node's storage shape, not part of the protocol — a Postgres backend would index the
array columns with GIN and add a pgvector embedding column; the wire contract (spec/commons.md) is
unchanged either way.
"""

from django.db import models


class Record(models.Model):
    # Content-address, e.g. "fn_3a9b…". Unique; the protocol's identity. (id is the sync sequence.)
    hash = models.CharField(max_length=128, unique=True, db_index=True)
    kind = models.CharField(max_length=32)            # function-record | message | body | type | certification
    schema_version = models.CharField(max_length=16)
    raw = models.JSONField()                          # the exact record bytes (as parsed JSON)

    # Certification records (null for other kinds): `subject` is the `fn_…` this certification attests to
    # (indexed, so "certifications about this function" is a keyed lookup), and `certified` its verdict.
    subject = models.CharField(max_length=128, null=True, blank=True, db_index=True)
    certified = models.BooleanField(null=True, blank=True)

    # Extracted, queryable fields (function records; null/empty for other kinds).
    effects = models.JSONField(default=list)
    capabilities = models.JSONField(default=list)
    intent_tags = models.JSONField(default=list)
    name_hints = models.JSONField(default=list)
    terminates = models.CharField(max_length=16, null=True, blank=True)
    complexity = models.CharField(max_length=64, null=True, blank=True)
    type_str = models.TextField(null=True, blank=True)
    body_hash = models.CharField(max_length=128, null=True, blank=True)

    # Body storage tiering (spec/commons.md open question 4): a bare body larger than the record cap
    # keeps only this POINTER in the metadata index — its canonical JSON bytes live in the blob store
    # under their sha256 (`raw` is then `{}`). Resolve streams the blob; everything else about the
    # gate (verify-then-store, self-addressing, idempotency) is unchanged. Null for inline rows.
    blob_sha256 = models.CharField(max_length=64, null=True, blank=True)
    blob_bytes = models.BigIntegerField(null=True, blank=True)

    # Semantic-search vector (spec/commons.md `POST /v0/search`). The L2-normalized embedding and the
    # id of the model that produced it (so a model change is detectable and rows can be re-embedded).
    # On a Postgres backend this becomes a pgvector column with an ANN index; here it is plain JSON
    # and cosine is computed in Python over a bounded scan (mirrors how query.py applies array preds).
    embedding = models.JSONField(null=True, blank=True)
    embedding_model = models.CharField(max_length=64, null=True, blank=True)

    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        ordering = ["id"]
        indexes = [
            models.Index(fields=["kind"]),
            models.Index(fields=["terminates"]),
        ]

    def __str__(self):
        return self.hash


class Anchor(models.Model):
    """A signed Merkle-root anchor of the corpus (commons.md open question 2; commons/anchor.py).
    The node's OWN history of what it attested holding — served at GET /v0/anchors. The copy that
    makes tampering evident is the one the operator pipes into an external append-only log; this
    table is the staging record and serving surface, not the trust root."""

    at = models.DateTimeField(auto_now_add=True)
    root = models.CharField(max_length=80)
    count = models.IntegerField()
    payload = models.JSONField()  # the full signed anchor statement

    class Meta:
        ordering = ["-id"]

    def __str__(self):
        return f"{self.root} @ {self.at}"


class Witness(models.Model):
    """A countersigned PEER anchor (commons.md open question 2, the federated half;
    commons/witness.py). This node verified the origin's anchor signature — and, when its own
    replicated corpus computed the same Merkle root, that agreement — and signed a statement
    embedding the origin anchor verbatim. Served at GET /v0/witnesses: the copy of the ORIGIN'S
    history the origin cannot rewrite. Append-only in spirit: an agreement upgrade adds a row."""

    at = models.DateTimeField(auto_now_add=True)
    origin = models.CharField(max_length=200)    # peer base URL the anchor was fetched from
    producer = models.CharField(max_length=120)  # the origin anchor's signer (did:nova)
    root = models.CharField(max_length=80)
    agreement = models.CharField(max_length=16)  # "root-matched" | "unverified"
    payload = models.JSONField()                 # the full signed witness statement

    class Meta:
        ordering = ["-id"]

    def __str__(self):
        return f"{self.origin} {self.root} ({self.agreement})"
