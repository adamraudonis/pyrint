import dataclasses


class FrozenOrThawed(type):
    def _make(cls, name, class_fields):
        cls._dataclass = dataclasses.make_dataclass(name, class_fields, frozen=True)

    def __init__(cls, name, bases, namespace, **kwargs):
        cls.__init__ = cls._dataclass.__init__
        cls.__new__ = object.__new__
