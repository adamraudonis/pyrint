class Index:
    def _convert_slice_indexer(self, key, kind):
        if kind == "getitem":
            if key.start:
                return key
            elif self.dtype.kind in "iu":
                return key
        if key.step:
            indexer = key
        else:
            indexer = self.slice_indexer(key)
        return indexer


def f(i, where):
    slobj = slice(None, where)
    s = Index()._convert_slice_indexer(slobj, kind="getitem")
    return s
