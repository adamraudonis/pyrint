import dataclasses


# attribute-form decorator: _looks_like_dataclass_decorator matches the
# Attribute path (astroid/brain/brain_dataclasses.py)
@dataclasses.dataclass
class A:
    x: int
    y: str = "hi"


@dataclasses.dataclass(init=False)
class NoInit:
    x: int


@dataclasses.dataclass
class WithFactory:
    items: list = dataclasses.field(default_factory=list)
