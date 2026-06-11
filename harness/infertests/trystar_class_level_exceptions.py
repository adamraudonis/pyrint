class CoordErr(Exception):
    pass


class AuthErr(Exception):
    pass


def f():
    try:
        pass
    except* (CoordErr, AuthErr) as eg:
        x = eg.exceptions
        return x
    return None


def g():
    try:
        pass
    except* ValueError as eg2:
        y = eg2.exceptions
        return y
    return None


def h():
    eg3 = ExceptionGroup("m", [ValueError()])
    return eg3.exceptions
