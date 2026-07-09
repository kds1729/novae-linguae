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
