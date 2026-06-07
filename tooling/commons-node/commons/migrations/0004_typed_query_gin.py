"""Postgres-only: GIN indexes for in-database typed query (DEPLOYMENT.md).

The typed `query` array predicates (effects / capabilities / intent_tags membership) are JSONB
containment / key-existence tests. A GIN index with `jsonb_path_ops` makes `@>` (contains) and the
overlap form fast, so on Postgres the predicates are pushed into the database (commons/query.py)
instead of scanned in Python. Vendor-guarded so SQLite stays a clean no-op — the Python post-filter
in query.py remains authoritative on every backend, with the index purely an optimization.
"""

from django.db import migrations

_COLUMNS = ["effects", "capabilities", "intent_tags"]


def create_gin(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    with schema_editor.connection.cursor() as cur:
        for col in _COLUMNS:
            cur.execute(
                f"CREATE INDEX IF NOT EXISTS commons_record_{col}_gin "
                f"ON commons_record USING gin ({col} jsonb_path_ops)"
            )


def drop_gin(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return
    with schema_editor.connection.cursor() as cur:
        for col in _COLUMNS:
            cur.execute(f"DROP INDEX IF EXISTS commons_record_{col}_gin")


class Migration(migrations.Migration):
    dependencies = [("commons", "0003_pgvector")]
    operations = [migrations.RunPython(create_gin, drop_gin)]
