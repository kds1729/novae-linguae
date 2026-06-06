"""Postgres-only: the pgvector ANN structures for semantic search.

Guarded by the DB vendor so it is a clean no-op on SQLite — the zero-dependency path and the test
suite keep working unchanged, and the same migration set applies on both backends. The portable
`embedding` JSONField (migration 0002) stays the source of truth; `embedding_vec` is a Postgres-only
physical column kept in sync by commons.vectorindex.store_vector, queried via pgvector's `<=>`.

Dimension comes from COMMONS_EMBEDDING_DIM (256 lexical / 384 for bge-small). Changing the embedding
model to a different dimension means re-running this (drop/recreate) and `embedrecords --all` — the
normal "re-embed everything" any vector DB requires on a model change.
"""

from django.conf import settings
from django.db import migrations


def _dim():
    return int(getattr(settings, "COMMONS_EMBEDDING_DIM", 256))


def create_pgvector(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    with schema_editor.connection.cursor() as cur:
        cur.execute("CREATE EXTENSION IF NOT EXISTS vector")
        cur.execute(f"ALTER TABLE commons_record ADD COLUMN IF NOT EXISTS embedding_vec vector({_dim()})")
        cur.execute(
            "CREATE INDEX IF NOT EXISTS commons_record_embedding_vec_hnsw "
            "ON commons_record USING hnsw (embedding_vec vector_cosine_ops)"
        )


def drop_pgvector(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    with schema_editor.connection.cursor() as cur:
        cur.execute("DROP INDEX IF EXISTS commons_record_embedding_vec_hnsw")
        cur.execute("ALTER TABLE commons_record DROP COLUMN IF EXISTS embedding_vec")


class Migration(migrations.Migration):
    dependencies = [("commons", "0002_record_embedding_record_embedding_model")]
    operations = [migrations.RunPython(create_pgvector, drop_pgvector)]
