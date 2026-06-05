"""WSGI entry point for the commons node (for gunicorn/uvicorn in a real deployment)."""

import os

from django.core.wsgi import get_wsgi_application

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "commons_node.settings")
application = get_wsgi_application()
