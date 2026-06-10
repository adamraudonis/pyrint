class CM:
    def __enter__(self):
        return self
    def __exit__(self, *a):
        return False


class CM2:
    def __enter__(self):
        return open("x")
    def __exit__(self, *a):
        return False


def use():
    with CM() as c, CM2() as f:
        pass
    return c, f


def gen_func():
    yield 1
    yield "two"


def use_gen():
    g = gen_func()
    for v in g:
        pass
    return v
