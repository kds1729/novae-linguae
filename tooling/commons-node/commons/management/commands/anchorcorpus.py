"""Sign a Merkle-root anchor of the corpus and print it (commons.md open question 2).

    python3 manage.py anchorcorpus            # store + print (skips if the root is unchanged)
    python3 manage.py anchorcorpus --force    # anchor even if unchanged
    python3 manage.py anchorcorpus >> /some/external/append-only/log.jsonl

The printed statement is the artifact the operator pipes into an EXTERNAL append-only log (a
public git repo, a transparency log, …) — the node's own /v0/anchors history is the staging
record, not the trust root. Verify any anchor by recomputing the root (from /v0/sync/merkle, a
mirror, or a bundle) and checking the Ed25519 signature (nl_crypto.verify_manifest).
"""

import json

from django.core.management.base import BaseCommand, CommandError

from commons.anchor import record_anchor


class Command(BaseCommand):
    help = "Sign + store + print a Merkle-root anchor of the corpus (needs COMMONS_ANCHOR_SEED)."

    def add_arguments(self, parser):
        parser.add_argument("--force", action="store_true",
                            help="anchor even when the root is unchanged since the last anchor")

    def handle(self, *args, **options):
        try:
            payload = record_anchor(force=options["force"])
        except RuntimeError as exc:
            raise CommandError(str(exc))
        if payload is None:
            from django.conf import settings
            if not settings.COMMONS_ANCHOR_SEED:
                raise CommandError("anchoring is not configured (set COMMONS_ANCHOR_SEED)")
            self.stderr.write("root unchanged since the last anchor; nothing emitted (--force overrides)")
            return
        self.stdout.write(json.dumps(payload, sort_keys=True, separators=(",", ":")))
