"""Egress-budget governor (DEPLOYMENT.md) — a hard ceiling on the only variable cost.

The VM fee is flat; metered *egress* (data out) is the one thing that can run away. This middleware
counts response bytes into a per-calendar-month counter (the window IONOS and most clouds bill on) and
returns ``503`` once a configured byte budget is reached — degrading gracefully instead of surprising
the operator with a bill. It is a principle-7 *endpoint policy* (a local choice of this node), never a
protocol gate: the wire contract in spec/commons.md is unchanged, and any other node may set any budget
or none.

Config (settings, all env-driven):
  - ``COMMONS_EGRESS_BUDGET_BYTES`` — the monthly ceiling. ``0`` (default) disables throttling, but the
    counter still runs, so usage is observable via ``/v0/info`` ("log-only" mode).
  - the counter lives in the Django cache (Redis in production, shared across web workers); the key is
    ``egress:<YYYY-MM>`` so it resets each calendar month on its own TTL.

Only egress is metered — incoming traffic is free on every cloud — so request bodies are never counted.
Immutable ``resolve`` responses fronted by an off-box CDN never reach this middleware, so CDN-absorbed
traffic correctly does not count against the budget.
"""

import datetime

from django.conf import settings
from django.core.cache import cache
from django.http import JsonResponse

_TTL = 35 * 24 * 3600  # > 1 month, so the calendar-month key always outlives its window


def _window_key(now=None):
    now = now or datetime.datetime.now(datetime.timezone.utc)
    return f"egress:{now:%Y-%m}"


def usage(now=None):
    """Current month's (used_bytes, budget_bytes, window) — for /v0/info and ops."""
    key = _window_key(now)
    return (int(cache.get(key) or 0), int(getattr(settings, "COMMONS_EGRESS_BUDGET_BYTES", 0)), key)


def _add(n_bytes, now=None):
    key = _window_key(now)
    # add() is a no-op if the key exists, so the counter survives concurrent workers; incr is atomic
    # on the Redis backend. On locmem (dev) it is per-process, which is fine for a single dev server.
    cache.add(key, 0, _TTL)
    try:
        return cache.incr(key, n_bytes)
    except ValueError:
        # Key expired between add() and incr() (month rollover race) — reseed and retry once.
        cache.add(key, 0, _TTL)
        return cache.incr(key, n_bytes)


class EgressBudgetMiddleware:
    """Throttle once the monthly egress budget is exhausted; always meter usage."""

    def __init__(self, get_response):
        self.get_response = get_response
        self.budget = int(getattr(settings, "COMMONS_EGRESS_BUDGET_BYTES", 0))

    def __call__(self, request):
        used, budget, _ = usage()
        if budget and used >= budget:
            resp = JsonResponse(
                {"error": "egress_budget_exhausted",
                 "detail": "this node has reached its monthly egress budget; try a mirror or retry next month"},
                status=503,
            )
            resp["Retry-After"] = "3600"
            return self._meter(resp)
        return self._meter(self.get_response(request))

    def _meter(self, response):
        if not getattr(response, "streaming", False):
            try:
                n = len(response.content)
            except Exception:
                n = 0
            if n:
                total = _add(n)
                if self.budget:
                    response["X-Egress-Budget"] = str(self.budget)
                    response["X-Egress-Used"] = str(total)
        return response
