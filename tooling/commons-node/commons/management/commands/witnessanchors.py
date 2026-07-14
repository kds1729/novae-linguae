"""Fetch a peer's signed anchors, verify + countersign them, and print the new witness
statements (commons.md open question 2, the federated half).

    python3 manage.py witnessanchors https://nl.example.com

Each printed statement is signed with THIS node's anchor identity and embeds the origin's full
signed anchor verbatim — a third party verifies both signatures with nl_crypto.verify_manifest
and needs neither node's honesty. `agreement: "root-matched"` additionally states this node's
own corpus computed the same Merkle root at witnessing time (run after replication has
converged for that). The beat task (`witness_anchors`, in replicate_all) does the same for every
configured peer; this command is the operator's manual/one-shot form.
"""

import json

from django.core.management.base import BaseCommand, CommandError


class Command(BaseCommand):
    help = "Verify + countersign a peer's Merkle-root anchors (needs COMMONS_ANCHOR_SEED)."

    def add_arguments(self, parser):
        parser.add_argument("peer", help="peer node base URL (its /v0/anchors is fetched)")

    def handle(self, *args, **options):
        from django.conf import settings

        from commons.models import Witness
        from commons.tasks import witness_anchors

        if not settings.COMMONS_ANCHOR_SEED:
            raise CommandError("witnessing is not configured (set COMMONS_ANCHOR_SEED)")
        before = Witness.objects.order_by("-id").first()
        summary = witness_anchors(options["peer"])
        if "error" in summary:
            raise CommandError(f"peer fetch failed: {summary['error']}")
        new = Witness.objects.order_by("id")
        if before:
            new = new.filter(id__gt=before.id)
        for row in new:
            self.stdout.write(json.dumps(row.payload, sort_keys=True, separators=(",", ":")))
        self.stderr.write(json.dumps(summary))
