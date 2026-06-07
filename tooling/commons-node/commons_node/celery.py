"""Celery app for the commons node's async worker (DEPLOYMENT.md).

The worker's headline job is **replication**: polling peers' ``GET /v0/sync`` and mirroring verified
records (commons/tasks.py). Because every record is self-verifying, a peer is untrusted — the worker
re-runs the same admission gate as a direct publish, so a malicious peer cannot inject anything the
node would not have accepted itself (principle 7).

Started in production as ``celery -A commons_node worker -B`` (``-B`` embeds beat). The broker is the
node's Redis (``COMMONS_CELERY_BROKER``); with no broker configured the worker simply isn't run and the
node is a pure origin. The web process never imports Celery, so the zero-dependency dev path is intact.
"""

import os

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "commons_node.settings")

from celery import Celery  # noqa: E402
from django.conf import settings  # noqa: E402

app = Celery("commons_node")
app.conf.broker_url = settings.COMMONS_CELERY_BROKER or "memory://"
app.conf.result_backend = None
app.conf.timezone = "UTC"
app.conf.beat_schedule = {
    "replicate-peers": {
        "task": "commons.tasks.replicate_all",
        "schedule": settings.COMMONS_REPLICATE_INTERVAL,
    },
    "embed-pending": {
        "task": "commons.tasks.embed_pending",
        "schedule": settings.COMMONS_EMBED_INTERVAL,
    },
}
app.autodiscover_tasks()
