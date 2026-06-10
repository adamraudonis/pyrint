class _Z:
    def __call__(self, *a, **kw):
        return self


class H:
    z = _Z()


h = H()


def use():
    x = h.z()
    y = h.z(root=None)
    w = _Z()()
    return x, y, w
