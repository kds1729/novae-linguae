#!/usr/bin/env python3
"""Django management entry point for the Novae Linguae commons reference node."""

import os
import sys


def main():
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "commons_node.settings")
    try:
        from django.core.management import execute_from_command_line
    except ImportError as exc:  # pragma: no cover
        raise ImportError(
            "Django is not installed. Install it (e.g. `pip install --user django`) "
            "or activate the project's virtualenv."
        ) from exc
    execute_from_command_line(sys.argv)


if __name__ == "__main__":
    main()
