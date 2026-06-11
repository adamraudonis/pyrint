import types

conf = types.ModuleType("conf")
conf.__file__ = "fn"
conf.x = 1
d = conf.__dict__


def myfunc():
    pass


myfunc.attr = 2
fd = myfunc.__dict__
