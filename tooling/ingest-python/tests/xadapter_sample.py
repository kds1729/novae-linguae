"""Cross-adapter fixture (Python side). Each function is written to translate to the SAME Nova
Lingua body as its Rust twin in `xadapter_sample.rs`, so the two adapters must agree byte-for-byte
on the body content-address. Consumed by `TestCrossAdapterAgreement`."""


def double(n: int) -> int:
    """Double a number.

    >>> double(5)
    10
    """
    return n + n


def times2(n: int) -> int:
    """Twice a number (a literal operand).

    >>> times2(5)
    10
    """
    return n * 2


def is_pos(n: int) -> bool:
    """Whether a number is positive.

    >>> is_pos(3)
    True
    >>> is_pos(-1)
    False
    """
    return n > 0


def safe_div(a: int, b: int) -> int | None:
    """Divide, or nothing on a zero divisor (a Maybe: None vs Some/Just).

    >>> safe_div(6, 2)
    3
    >>> safe_div(7, 0)
    """
    if b == 0:
        return None
    return a // b


def add_sub(a: int, b: int) -> tuple[int, int]:
    """The sum and the difference (a tuple result).

    >>> add_sub(5, 3)
    (8, 2)
    """
    return (a + b, a - b)
