"""GW8 — real code into the commons (spec/expressiveness.md).

A small, ordinary, fully-annotated Python inventory module: exactly the kind of code the
ingestion statement subset now covers (the None<->Maybe boundary, search loops, guarded and
nested accumulator loops). `nl-ingest-py --v2 --emit-dir` lifts every function here to an
executable record, `nl-validator certify` verifies each one, and the records + signed
certifications publish to a commons node — real library code becoming discoverable, trusted,
runnable commons artifacts with no hand-authoring step.
"""


def lookup_qty(stock: dict[str, int], item: str) -> int | None:
    """The quantity of an item in stock, if it is stocked at all.

    >>> lookup_qty({"apples": 3, "pears": 0}, "apples")
    3
    >>> lookup_qty({"apples": 3}, "plums")
    """
    return stock.get(item)


def qty_or_zero(q: int | None) -> int:
    """A quantity that may be missing, defaulted to zero.

    >>> qty_or_zero(7)
    7
    >>> qty_or_zero(None)
    0
    """
    if q is None:
        return 0
    return q


def first_short(levels: list[int], cutoff: int) -> int | None:
    """The first stock level below the reorder cutoff, if any.

    >>> first_short([9, 4, 12], 5)
    4
    >>> first_short([9, 12], 5)
    """
    for level in levels:
        if level < cutoff:
            return level
    return None


def total_units(levels: list[int]) -> int:
    """Total units on hand across all stock levels.

    >>> total_units([3, 0, 7])
    10
    >>> total_units([])
    0
    """
    total = 0
    for level in levels:
        total = total + level
    return total


def low_stock_count(levels: list[int], cutoff: int) -> int:
    """How many stock levels sit below the reorder cutoff.

    >>> low_stock_count([9, 4, 12, 1], 5)
    2
    >>> low_stock_count([9], 5)
    0
    """
    n = 0
    for level in levels:
        if level < cutoff:
            n += 1
    return n


def all_levels(sections: list[list[int]]) -> list[int]:
    """Every stock level across all warehouse sections, in order.

    >>> all_levels([[3, 0], [7]])
    [3, 0, 7]
    >>> all_levels([])
    []
    """
    out = []
    for section in sections:
        for level in section:
            out.append(level)
    return out
