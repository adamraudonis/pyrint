def f1():
    x, y = __foo__()
    use(x)
def f2():
    a, b = __foo__["k"](1)
    use(a)
def f3():
    c = __foo__
    use(c)
def f4():
    d, e = __foo__
    use(d)
def f5():
    g = __foo__()
    use(g)
def f6():
    h, i = (__foo__(), 2)
    use(h)
