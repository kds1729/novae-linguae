"""Backfill / refresh semantic-search embeddings (spec/commons.md `POST /v0/search`).

Embeds every record whose embedding is missing or was produced by a model other than the node's
current one — so it both populates records ingested before search existed and re-embeds cleanly
after a `COMMONS_EMBEDDER` change. Idempotent: a second run with no model change is a no-op.

    python3 manage.py embedrecords            # only records missing the current model's embedding
    python3 manage.py embedrecords --all      # re-embed everything
"""

from django.core.management.base import BaseCommand

from commons.embedding import get_embedder
from commons.models import Record
from commons.vectorindex import store_vector


class Command(BaseCommand):
    help = "Compute embeddings for records missing them or embedded by a different model."

    def add_arguments(self, parser):
        parser.add_argument("--all", action="store_true",
                            help="re-embed every record, even if already at the current model")
        parser.add_argument("--batch", type=int, default=500, help="bulk_update batch size")

    def handle(self, *args, **options):
        emb = get_embedder()
        qs = Record.objects.all().order_by("id")
        if not options["all"]:
            qs = qs.exclude(embedding_model=emb.model_id)   # null and stale-model rows remain
        total = qs.count()

        pending = list(qs)            # MVP scale; chunk so a remote model server batches efficiently
        size = max(1, options["batch"])
        done = 0
        for i in range(0, len(pending), size):
            chunk = pending[i:i + size]
            vectors = emb.embed_batch([r.raw for r in chunk])
            for r, v in zip(chunk, vectors):
                r.embedding = v
                r.embedding_model = emb.model_id
            Record.objects.bulk_update(chunk, ["embedding", "embedding_model"])
            for r in chunk:
                store_vector(r.hash, r.embedding)   # sync pgvector column on Postgres; no-op on SQLite
            done += len(chunk)

        self.stdout.write(f"embedded={done} of {total} pending  model={emb.model_id}")
