class Cell:
    def __init__(self, a, b):
        pass

def f():
    cells = [
        c1 := Cell("x", 1),
        c2 := Cell("y", 2),
    ]
    with g(cells, c1):
        pass

def g(a, b):
    return a

def h():
    xs = [
        a1 := unknown_fn("x"),
        a2 := Cell("y", 2),
    ]
    with g(xs, a1):
        pass

def st():
    ys = [*unknown_fn2(), 3]
    return ys
