from typing import Protocol


class CacheProtocol[T, U](Protocol):
    def __getitem__(self, key: T) -> U: ...


class BoundedCache[T, U](CacheProtocol[T, U]):
    pass


class UnreferencedGeneric[X]:
    pass


class Plain:
    pass


class UsesPlain(Plain):
    pass
