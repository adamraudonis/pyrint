def f(x=None):
    return [1] if x else None

a = "v=%s" % (f(),)
b = "v=%s w=%s" % ("a", f())
c = "n=%(n)s" % {"n": 3}
d = "n=%(n)s" % {"n": f()}
e = "%s" % None
g = "x" % ()
h = "%d" % ("a",)
i = "%(a)s %(b)d" % {"a": "x", "b": 2}
j = "v=%s" % ("ok",)
