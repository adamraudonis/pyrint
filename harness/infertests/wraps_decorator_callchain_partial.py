import functools


class Disallow:
    def __call__(self, f):
        @functools.wraps(f)
        def _f(*args, **kwargs):
            print(args)
            return f(*args, **kwargs)

        return _f
