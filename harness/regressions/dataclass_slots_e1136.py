from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class Stats:
    size: int
    mean: float


def check():
    st = Stats(size=5, mean=2.0)
    return st[0]
