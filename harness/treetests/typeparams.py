from typing import Callable


def invalid_schema[**P](func: Callable[P, None]) -> Callable[P, None]:
    return func


def tv[T, *Ts](x: T) -> T:
    return x


class Box[T: int](list):
    pass


type Alias[T] = list[T]
