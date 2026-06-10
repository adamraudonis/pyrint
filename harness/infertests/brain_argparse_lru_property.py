import argparse
import functools


def get_prop(self):
    return 4


class FP:
    prop = property(get_prop)


@functools.lru_cache()
def square(x):
    return x * x


square.cache_clear()
n = argparse.Namespace(a=1, b=2)
p = FP().prop
