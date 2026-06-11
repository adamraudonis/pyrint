from enum import StrEnum, Enum, IntEnum


class UnitOfEnergy(StrEnum):
    KILO_WATT_HOUR = "kWh"
    WATT_HOUR = "Wh"


class Color(Enum):
    RED = 1


class Level(IntEnum):
    LOW = 1


PRICE = f"EUR/{UnitOfEnergy.KILO_WATT_HOUR}"
print(PRICE, UnitOfEnergy.KILO_WATT_HOUR, Color.RED, Level.LOW)
