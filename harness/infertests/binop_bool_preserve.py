class A:
    def __init__(self, negated=False):
        self.negated = negated

    def f(self, negated):
        x = negated ^ self.negated
        return x

def g(flag):
    a = True
    if flag:
        a = False
    b = flag ^ a
    c = a ^ True
    d = a & a
    e = a | False
    return b, c, d, e
