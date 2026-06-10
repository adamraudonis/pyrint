import attrs
import attr
from typing import Any, ClassVar

@attrs.define(kw_only=True, repr=False)
class OperatorPartial:
    operator_class: type
    kwargs: dict
    params: dict
    registry: ClassVar[int] = 0
    _expand_called: bool = False

    def expand(self):
        a = self.operator_class
        b = self.kwargs
        c = self._expand_called
        return a, b, c

@attr.s
class Old:
    x = attr.ib(default=1)
    y = 5

o = Old()
o.x
o.y
p = OperatorPartial(operator_class=type, kwargs={}, params={})
p.operator_class
p.kwargs
p._expand_called
p.registry
