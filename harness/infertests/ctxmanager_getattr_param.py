import sys
from contextlib import contextmanager


def f(stream_name):
    return getattr(sys, stream_name)


x = f("stderr")
y = getattr(sys, "stdout")


@contextmanager
def g(stream_name):
    yield getattr(sys, stream_name)


def use3():
    with g("stdout") as out:
        pass
    return out


@contextmanager
def h(name):
    yield name


def use4():
    with h("zz") as nm:
        pass
    return nm
