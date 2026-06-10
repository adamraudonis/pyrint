import enum
import attr
from enum import Enum, IntEnum
from dataclasses import dataclass, field
from hypothesis import strategies as st


class AxisOrder(IntEnum):
    TRADITIONAL = 0
    AUTHORITY = 1


class Color(enum.Enum):
    RED = "red"


class Indirect(Color):
    pass


class NotEnum(object):
    A = 1


class WithName(Enum):
    name = "x"
    other = "y"


@dataclass
class Config:
    section: str = "core"
    items: list = field(default_factory=list)


@dataclass(init=False)
class NoInit:
    a: int = 0


@dataclass(frozen=True)
class Frozen:
    b: str = ""


@dataclass
class HasInit:
    c: int = 0

    def __init__(self):
        self.c = 1


@attr.s(slots=True)
class Group:
    name = attr.ib()


@st.composite
def a_strategy(draw, extra):
    return draw(st.integers())
