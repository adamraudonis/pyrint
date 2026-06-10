def Abstract(m, q):
    return (m, q)

class BaseSemverTest:
    __test__ = Abstract(__module__, __qualname__)
    a = __module__
    b = __qualname__

class Sub(BaseSemverTest):
    c = __module__

x = BaseSemverTest.__test__
x
