"""Sample module for exercising nl-ingest-py. Not part of Novae Linguae itself."""

from typing import Optional, TypeVar, Union

T = TypeVar("T")
U = TypeVar("U")

__all__ = ["double", "to_upper", "lookup", "first", "maybe_parse", "combine", "no_annotations"]


def double(n: int) -> int:
    return n + n


def to_upper(s: str) -> str:
    return s.upper()


def lookup(table: dict[str, int], key: str) -> Optional[int]:
    return table.get(key)


def first(xs: list[T]) -> Optional[T]:
    return xs[0] if xs else None


def maybe_parse(text: str) -> int | None:
    try:
        return int(text)
    except ValueError:
        return None


def combine(f, g, x: T) -> U:  # f, g unannotated -> 'unknown'
    return g(f(x))


def no_annotations(a, b):
    return a + b


def _private_helper(x: int) -> int:  # excluded: not in __all__ and _-prefixed
    return x * 2


async def fetch(url: str) -> bytes:  # excluded by __all__, but reachable via --include-private
    return b""
