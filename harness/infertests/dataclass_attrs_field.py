import dataclasses
from typing import Optional, ClassVar

@dataclasses.dataclass
class IOArgs:
    encoding: Optional[str]
    mode: str
    registry: ClassVar[int] = 0

a = IOArgs(encoding=None, mode="wb")
a.encoding
a.mode
a.registry

@dataclasses.dataclass
class WithDefaults:
    x: int = 5
    y: list = dataclasses.field(default_factory=list)
    z: "Optional[IOArgs]" = None

w = WithDefaults()
w.x
w.y
w.z
