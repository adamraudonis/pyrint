import functools
from functools import lru_cache


@functools.lru_cache(maxsize=None)
def iter_modules_and_files(modules, extra_files):
    return [1, 2]


@lru_cache(maxsize=512)
def getv(k):
    return {}


def f():
    a = iter_modules_and_files.cache_info()
    b = getv.cache_clear()
    c = getv("x")
    return a, b, c
