dist = None  # type: int
other = None  # type: ignore
third = None  #type:str
multi = (1,
         2)  # type: tuple


def perarg(a,  # type: int
           b,  # type: str
           ):
    # type: (...) -> None
    pass


def functype(a, b):
    # type: (int, str) -> None
    pass
