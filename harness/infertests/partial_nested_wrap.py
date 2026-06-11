import functools


def async_with_args(a, b, c):
    return a


fn = functools.partial(functools.partial(async_with_args, 1), 2)
x = fn
y = fn(3)
