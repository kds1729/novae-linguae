"""Sample library module of pure functions whose bodies are in the executable subset (conditionals,
local bindings, arithmetic, a mapped builtin) AND that carry doctests — so `nl-ingest-py --v2
--emit-dir` produces records with real examples and runnable body ASTs that `nl-validator run` can
execute. Used by the executable-corpus round-trip test."""


def clamp(x, lo, hi):
    """Clamp x into the inclusive range [lo, hi].

    >>> clamp(5, 0, 10)
    5
    >>> clamp(-3, 0, 10)
    0
    >>> clamp(99, 0, 10)
    10
    """
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x


def sign(n):
    """The sign of n: -1, 0, or 1.

    >>> sign(5)
    1
    >>> sign(-2)
    -1
    >>> sign(0)
    0
    """
    if n > 0:
        return 1
    if n < 0:
        return -1
    return 0


def abs_diff(a, b):
    """Absolute difference of a and b (local binding + the mapped `abs` builtin).

    >>> abs_diff(3, 7)
    4
    >>> abs_diff(7, 3)
    4
    """
    d = a - b
    return abs(d)


def squares(xs):
    """Square each element (list comprehension -> map).

    >>> squares([1, 2, 3])
    [1, 4, 9]
    >>> squares([])
    []
    """
    return [x * x for x in xs]


def total(xs):
    """Sum a list (accumulator loop -> foldl).

    >>> total([1, 2, 3, 4])
    10
    >>> total([])
    0
    """
    acc = 0
    for x in xs:
        acc = acc + x
    return acc
