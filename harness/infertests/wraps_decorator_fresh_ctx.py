import functools


class Bottleneck:
    def __init__(self, alt):
        self.kwargs = {}

    def deco(self, alt):
        @functools.wraps(alt)
        def f(values, *, axis=None, skipna=True, **kwds):
            print(kwds)
            print(values)
            return alt(values, axis=axis, skipna=skipna, **kwds)

        return f
