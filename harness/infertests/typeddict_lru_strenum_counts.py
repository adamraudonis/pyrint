from collections.abc import Callable
import contextlib
from enum import StrEnum
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:

    def lru_cache[_T: Callable[..., Any]](func: _T) -> _T:
        """Stub for lru_cache."""

else:
    from functools import lru_cache


class DC(StrEnum):
    A = "a"
    B = "b"


@lru_cache
def try_parse_enum[_EnumT](cls: type[_EnumT], value: Any) -> _EnumT | None:
    with contextlib.suppress(ValueError):
        return cls(value)
    return None


def f(value):
    x = try_parse_enum(DC, value)
    return x
