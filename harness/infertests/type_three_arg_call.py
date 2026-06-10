X = type("X", (object,), {"a": 1, "b": 2})
X
X().a
Y = type("Y", (), {})
Y
class Meta(type):
    pass
def factory(name):
    return type(name)(name + "Form", (dict,), {"f": 3})
Z = factory("Zed")
Z
W = type("W", 3, {})
W
V = type("V" + somevar, (object,), {})
V
def mf():
    n = "Q"
    return type(n, (object,), {"m": lambda self: 5})
Q = mf()
Q
Q().m
