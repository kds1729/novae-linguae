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
    kind = models.CharField(max_length=32)            # function-record | message | body | type
    schema_version = models.CharField(max_length=16)
    raw = models.JSONField()                          # the exact record bytes (as parsed JSON)

    # Extracted, queryable fields (function records; null/empty for other kinds).
    effects = models.JSONField(default=list)
    capabilities = models.JSONField(default=list)
    intent_tags = models.JSONField(default=list)
    name_hints = models.JSONField(default=list)
    terminates = models.CharField(max_length=16, null=True, blank=True)
    complexity = models.CharField(max_length=64, null=True, blank=True)
    type_str = models.TextField(null=True, blank=True)
    body_hash = models.CharField(max_length=128, null=True, blank=True)

    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        ordering = ["id"]
        indexes = [
            models.Index(fields=["kind"]),
            models.Index(fields=["terminates"]),
        ]

    def __str__(self):
        return self.hash
