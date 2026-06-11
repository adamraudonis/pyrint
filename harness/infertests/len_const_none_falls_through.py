def f(indexer, target=None):
    if target is not None and isinstance(indexer, slice):
        target_len = len(target)
        return target_len
    return 0

a = f([1])
a
b = len(None)
b
c = len(5)
c
