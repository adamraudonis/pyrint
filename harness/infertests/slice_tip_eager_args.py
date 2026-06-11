def f(vals, t1, t2):
    left = vals.searchsorted(t1, side="left")
    right = vals.searchsorted(t2, side="right")
    return slice(left, right)


x = f([1], 2, 3)
