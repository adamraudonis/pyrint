def deco(arg):
    def w(f):
        return f
    return w


class C:
    backend = "x.y.Z"

    @deco(backend)
    def m(self):
        return backend


value = "mod"


class D:
    @deco(value)
    def m(self):
        pass
