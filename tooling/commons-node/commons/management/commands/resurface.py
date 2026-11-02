"""Backfill / refresh the surface-syntax decision fields of function records (`Record.surface`,
`verify.surface` — what `?include=summary` serves an agent choosing among candidates).

    python3 manage.py resurface            # only function records with no surface yet
    python3 manage.py resurface --all      # re-render every function record

Idempotent: rendering is a pure function of the stored record and the validator binary.
"""

from django.core.management.base import BaseCommand

from commons import verify as V
from commons.models import Record


class Command(BaseCommand):
    help = "Render function records' type, contract and laws to surface syntax for summaries."

    def add_arguments(self, parser):
        parser.add_argument("--all", action="store_true", help="re-render every function record")
        parser.add_argument("--batch", type=int, default=200, help="bulk_update batch size")

    def handle(self, *args, **options):
        qs = Record.objects.filter(kind="function-record").order_by("id")
        if not options["all"]:
            qs = qs.filter(surface={})
        rows = list(qs)
        size = max(1, options["batch"])
        rendered = 0
        for i in range(0, len(rows), size):
            chunk = rows[i:i + size]
            for r in chunk:
                r.surface = V.surface(r.raw, r.kind)
                rendered += bool(r.surface)
            Record.objects.bulk_update(chunk, ["surface"])
        self.stdout.write(f"resurfaced={len(rows)} rendered={rendered}")
