def pick(flag=None):
    if flag:
        return None
    return [1, 2]

def msg(x=None):
    v = pick(x)
    return f"value is {v}"

def msg2():
    d = {}
    l = []
    t = (1, 2)
    s = {1}
    return f"d={d} l={l} t={t} s={s}"

def gen():
    yield 1

def msg3():
    g = gen()
    return f"g={g}"

class C:
    def m(self):
        return 1


a = msg()
b = msg2()
c = msg3()
