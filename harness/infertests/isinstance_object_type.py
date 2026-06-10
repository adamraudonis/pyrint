import numpy as np

class A: ...
class B(A): ...

a = B()
r1 = isinstance(a, A)
r1
r2 = isinstance(a, (A, 1))
r2
r3 = isinstance([1], np.ndarray)
r3
def g(x):
    if x:
        y = B()
    else:
        y = "s"
    return isinstance(y, A)
r5 = isinstance(a, B if a else A)
r5
r6 = issubclass(B, A)
r6
