def f():
    conditions = []
    for entry in unknown():
        base = {"entity": entry.id}
        conditions += [
            {**base, "type": "disarmed"},
        ]
    return conditions


def g():
    xs = []
    for entry in unknown():
        xs += [entry.id]
    return xs


def h():
    ys = []
    base = {"entity": unknown_name.id}
    ys += [{**base, "t": 1}]
    return ys
