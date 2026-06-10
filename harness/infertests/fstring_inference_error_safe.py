import os


def f1(u):
    return f"{u:%Y}_{os.getpid()}"


def f2(u):
    return f"{u:%Y}_{u:%m}"


def f3(u):
    return f"{u:%Y}_"


def f4(u):
    return f"_{os.getpid()}"


def use():
    a = f1(1)
    b = f2(2)
    c = f3(3)
    d = f4(4)
    print(a, b, c, d)
