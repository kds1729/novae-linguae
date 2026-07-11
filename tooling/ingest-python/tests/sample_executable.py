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


def sum_positives(xs):
    """Sum the positive elements (guarded accumulator loop -> foldl with a case step).

    >>> sum_positives([1, -2, 3, -4, 5])
    9
    >>> sum_positives([-1, -2])
    0
    """
    total = 0
    for x in xs:
        if x > 0:
            total += x
    return total


def count_evens(xs):
    """Count the even elements (guarded count loop -> foldl).

    >>> count_evens([1, 2, 3, 4, 6])
    3
    >>> count_evens([1, 3])
    0
    """
    c = 0
    for x in xs:
        if x % 2 == 0:
            c += 1
    return c


def doubled(xs):
    """Double each element by building a list (append loop -> map).

    >>> doubled([1, 2, 3])
    [2, 4, 6]
    >>> doubled([])
    []
    """
    out = []
    for x in xs:
        out.append(x + x)
    return out


def keep_positive(xs):
    """Keep the positive elements (guarded append loop -> filter).

    >>> keep_positive([1, -2, 3, -4])
    [1, 3]
    >>> keep_positive([-1])
    []
    """
    out = []
    for x in xs:
        if x > 0:
            out.append(x)
    return out


def squares_of_evens(xs):
    """Square the even elements (guarded append loop -> map over filter).

    >>> squares_of_evens([1, 2, 3, 4])
    [4, 16]
    >>> squares_of_evens([1, 3])
    []
    """
    out = []
    for x in xs:
        if x % 2 == 0:
            out.append(x * x)
    return out


def first_negative(xs):
    """The first negative element, or 0 if none (early-return search loop -> filter/head).

    >>> first_negative([3, -4, 5, -6])
    -4
    >>> first_negative([1, 2])
    0
    """
    for x in xs:
        if x < 0:
            return x
    return 0


def contains(xs, target):
    """Whether target occurs in xs (early-return any loop).

    >>> contains([1, 2, 3], 2)
    True
    >>> contains([1, 2, 3], 5)
    False
    """
    for x in xs:
        if x == target:
            return True
    return False


def double_first_even(xs):
    """Twice the first even element, or -1 if none (search with a transformed hit).

    >>> double_first_even([3, 4, 5])
    8
    >>> double_first_even([1, 3])
    -1
    """
    for x in xs:
        if x % 2 == 0:
            return x * 2
    return -1


def sum_minus_count(xs):
    """Sum of the elements minus how many there are (independent two-accumulator loop -> two folds).

    >>> sum_minus_count([5, 6])
    9
    >>> sum_minus_count([])
    0
    """
    s = 0
    c = 0
    for x in xs:
        s += x
        c += 1
    return s - c


def even_sum_and_count(xs):
    """Sum plus count of the even elements (guarded two-accumulator loop).

    >>> even_sum_and_count([1, 2, 3, 4])
    8
    >>> even_sum_and_count([1, 3])
    0
    """
    s = 0
    c = 0
    for x in xs:
        if x % 2 == 0:
            s += x
            c += 1
    return s + c


def flatten(xss):
    """Concatenate a list of lists (nested list-building loop -> a foldl of appends).

    >>> flatten([[1, 2], [3]])
    [1, 2, 3]
    >>> flatten([])
    []
    """
    out = []
    for row in xss:
        for i in row:
            out.append(i)
    return out


def evens_of_rows(xss):
    """The even elements of every row, in order (nested loop with an inner guard).

    >>> evens_of_rows([[1, 2], [3, 4]])
    [2, 4]
    >>> evens_of_rows([[1]])
    []
    """
    out = []
    for row in xss:
        for i in row:
            if i % 2 == 0:
                out.append(i)
    return out


def or_default(x: int | None, d):
    """The value unless missing (`is None` narrowing -> case on the Maybe; a None argument
    encodes as the None variant at the example boundary).

    >>> or_default(5, 0)
    5
    >>> or_default(None, 7)
    7
    """
    if x is None:
        return d
    return x


def bump(x: int | None) -> int | None:
    """One more than x, if present (narrowing + a Just-wrapped return; returns None for None —
    which a doctest honestly cannot show, so only the present case carries an example).

    >>> bump(4)
    5
    """
    if x is None:
        return None
    return x + 1


def lookup_qty(d: dict, k) -> int | None:
    """The quantity stored under k, if any (bare 1-arg get -> map_get's Maybe, passed through
    the Optional return unwrapped).

    >>> lookup_qty({"apples": 3}, "apples")
    3
    """
    return d.get(k)


def find_big(xs, cutoff) -> int | None:
    """The first element above cutoff, if any (search loop returning a Maybe: the hit wraps in
    Just, the not-found default is the None variant).

    >>> find_big([1, 8, 3], 2)
    8
    """
    for x in xs:
        if x > cutoff:
            return x
    return None


def add_sub(a: int, b: int) -> tuple[int, int]:
    """Both the sum and the difference of two numbers (tuple RESULT construction).

    >>> add_sub(5, 3)
    (8, 2)
    >>> add_sub(2, 7)
    (9, -5)
    """
    return (a + b, a - b)


def swap_diff(a: int, b: int) -> int:
    """Swap two numbers into a tuple, then subtract — the first minus the second
    (tuple-unpacking assignment `x, y = (…)`; with x=b, y=a the result is a - b).

    >>> swap_diff(3, 10)
    -7
    >>> swap_diff(10, 3)
    7
    """
    x, y = (b, a)
    return y - x


def running_gap(xs: list[int]) -> int:
    """The negated running-sum total: subtract the running sum from a gap at each step — a
    DEPENDENT two-accumulator loop (the gap update reads the just-updated sum), expressible only
    with a tuple accumulator.

    >>> running_gap([1, 2, 3])
    -10
    >>> running_gap([])
    0
    """
    s = 0
    g = 0
    for x in xs:
        s = s + x
        g = g - s
    return g


def sum_values(pairs: list[tuple[int, int]]) -> int:
    """Sum the second component of each pair (tuple-unpacking `for (k, v) in …`).

    >>> sum_values([(1, 2), (3, 4)])
    6
    >>> sum_values([])
    0
    """
    total = 0
    for (k, v) in pairs:
        total = total + v
    return total


def keys_over(pairs: list[tuple[int, int]], cutoff: int) -> list[int]:
    """The first components whose second component exceeds the cutoff (tuple-unpacking `for` with
    a guarded append).

    >>> keys_over([(1, 9), (2, 3), (5, 8)], 5)
    [1, 5]
    >>> keys_over([(1, 2)], 5)
    []
    """
    out = []
    for (k, v) in pairs:
        if v > cutoff:
            out.append(k)
    return out


def per_unit(total: int, count: int) -> int:
    """Units per box, refusing an empty box count (raise-totalization: the record's result is
    `Maybe int`, the guard-raise is its None arm — and the Traceback doctest IS the runnable
    None-case example).

    >>> per_unit(12, 4)
    3
    >>> per_unit(5, 0)
    Traceback (most recent call last):
        ...
    ValueError: no boxes
    """
    if count == 0:
        raise ValueError("no boxes")
    return total // count


def label_of(s: str) -> str:
    """The string itself, or a placeholder when empty (str truthiness: `if s` desugars to
    `s != ""` — the falsy set is annotation-proven).

    >>> label_of("widget")
    'widget'
    >>> label_of("")
    '(unnamed)'
    """
    if s:
        return s
    return "(unnamed)"


def batch_size(xs: list[int]) -> int:
    """How many items, or -1 for no batch at all (list truthiness: `if xs` is `not (null xs)`).

    >>> batch_size([4, 5])
    2
    >>> batch_size([])
    -1
    """
    if xs:
        return len(xs)
    return -1


def scaled(n: int, factor: int) -> int:
    """n scaled by factor, defaulting a zero factor to identity (int truthiness: `if factor`
    is `factor != 0`).

    >>> scaled(7, 3)
    21
    >>> scaled(7, 0)
    7
    """
    if factor:
        return n * factor
    return n


def ready(name: str, count: int) -> bool:
    """Whether a named, non-empty order is ready (a MIXED test: truthy str `and` a comparison —
    strict connectives, the purity argument).

    >>> ready("box", 2)
    True
    >>> ready("", 2)
    False
    >>> ready("box", 0)
    False
    """
    if name and count > 0:
        return True
    return False
