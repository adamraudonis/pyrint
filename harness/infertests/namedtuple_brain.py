from collections import namedtuple
from typing import NamedTuple

Point = namedtuple("Point", ["x", "y"])
Point
p = Point(1, 2)
p
p.x
p._fields
Pair = namedtuple("Pair", "a b")
Pair
class Ax(NamedTuple):
    class Bx:
        b = 0
Ax
Ax.Bx.b
class Off(NamedTuple):
    name: str
    offset: int
Off
Off("x", 1)
NT2 = NamedTuple("NT2", [("q", int)])
NT2
Bad = namedtuple("Bad", ["def", "x"])
Bad
Ren = namedtuple("Ren", ["def", "x"], rename=True)
Ren
