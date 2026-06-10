dag_models = {}
unique_dag_ids = []


def f():
    rollup = [
        d_id for d_id in unique_dag_ids if (dm := dag_models.get(d_id)) is not None and dm.x
    ]
    s = {w for w in (c := [1]) if (q := w)}
    return rollup, s


g = lambda a=(w0 := 1): a
print(t0 := 5)


class K:
    val = [(kx := 2) for _ in range(2)]


class WithDecorators:
    @(lambda f: f)
    def plain(self):
        pass

    def deco_walrus(self, *, default=(dw := 3)):
        return dw
