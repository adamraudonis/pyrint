import functools

def _getpwall(root=None):
    yield 1
    yield 2

def f(root=None):
    if root is not None:
        getpwall = functools.partial(_getpwall, root=root)
    else:
        getpwall = functools.partial(_getpwall)
    return list(getpwall())
