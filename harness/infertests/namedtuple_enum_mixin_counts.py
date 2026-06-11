from collections import namedtuple
from enum import Enum


class Role(namedtuple("Role", "name order"), Enum):
    VIEWER = "VIEWER", 0
    USER = "USER", 1
    OP = "OP", 2


x = Role.OP
y = Role.VIEWER.order
