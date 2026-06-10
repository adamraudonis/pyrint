from dataclasses import dataclass
from collections.abc import Callable


@dataclass(frozen=True, kw_only=True)
class Desc:
    is_available_fn: Callable[[str, str], bool] = lambda device, key: (
        device.online
        and (sensor := device.sensors.get(key)) is not None
    )
    value_fn: str = ""


@dataclass
class Fine:
    a: int = 0
