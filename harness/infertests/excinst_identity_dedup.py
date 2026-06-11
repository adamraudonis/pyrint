class E(Exception):
    pass

def f(flag):
    try:
        return 1
    except E as ex:
        if flag:
            return ex
        return ex

r = f(0)
use(r)
