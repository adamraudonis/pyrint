from enum import Enum, StrEnum, IntFlag

class Color(Enum):
    RED = 1
    GREEN = "green"

Color.RED
Color.RED.value
Color.RED.name
Color.GREEN.value
Color.__members__
Color._value2member_map_

class Hdr(StrEnum):
    ENFORCE = "Content-Security-Policy"

Hdr.ENFORCE
Hdr.ENFORCE.value

class Fl(IntFlag):
    A = 1
    B = 2

Fl.A | Fl.B
x = Color.RED
x.name
