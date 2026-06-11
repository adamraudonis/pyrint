from dataclasses import dataclass, field
from nonexistent_module import SomeFactory


def my_factory():
    return {"a": 1}


@dataclass
class C:
    items: list = field(default_factory=list)
    mapping: dict = field(default_factory=dict)
    custom: object = field(default_factory=my_factory)
    missing: object = field(default_factory=SomeFactory)
    fixed: int = field(default=3)


c = C()
print(c.items, c.mapping, c.custom, c.missing, c.fixed)
