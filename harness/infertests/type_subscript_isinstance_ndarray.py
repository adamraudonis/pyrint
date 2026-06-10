import numpy as np

def f() -> type[dict]:
    return dict

x = type[int]
x
def g(a):
    if not isinstance(a, np.ndarray):
        return a.isin([1])
    return a
