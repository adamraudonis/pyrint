from enum import Enum


class Color(Enum):
    RED = "red"
    GREEN = "green"


def f(data):
    c = Color(data["action"])
    n = Color.__new__(Color, data["action"])
    m = Color._value2member_map_[data["action"]]
    cr = Color._missing_(data["action"])
    return c, n, m, cr
