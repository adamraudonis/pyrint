from dataclasses import dataclass
from pydantic.dataclasses import dataclass as pydantic_dataclass
from pydantic.dataclasses import dataclass as plain_named_dataclass2


@pydantic_dataclass
class NotADataclassForAstroid:
    a: int = 0


@dataclass
class RealOne:
    b: int = 0
