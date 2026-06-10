class HDFStore:
    def info(self):
        return f"{type(self)}\nFile path: x\n"


def fstr_func():
    def inner():
        pass
    return f"{inner} and {fstr_func}"


def fstr_mod():
    import os
    return f"{os}!"


class A:
    pass


def fstr_cls_short():
    return f"{A}"
