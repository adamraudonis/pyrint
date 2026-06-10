class Expression:
    def __repr__(self):
        return f"({self})"

def f(x):
    s = f"Instance: {x}"
    return s

a = Expression()
m1 = f"val {a} end"
m1
m2 = f"{a}"
m2
def g(u):
    m3 = f"{u}_and_more"
    return m3
m4 = f"{a!r:>10}"
m4
m5 = f"{1+1} {None} {True}"
m5
m6 = f""
m6
