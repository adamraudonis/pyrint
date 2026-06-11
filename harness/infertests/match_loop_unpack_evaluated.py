from collections.abc import Iterable, Iterator, Mapping


def normalize_choices(value, *, depth=0):
    match value:
        case bytes() | str():
            return value
        case Mapping() if depth < 2:
            value = value.items()
        case Iterator() if depth < 2:
            pass
        case Iterable() if depth < 2:
            value = [normalize_choices(x, depth=depth + 1) for x in value]

    return value


choices = {"C": 1, "D": 2, "H": 3, "S": 4}
out = normalize_choices(choices)
