import pickle

class Op:
    def _copy(self, src):
        lib = getattr(self, "pickling_library", pickle)
        return lib.load(src)

def f():
    return pickle.loads(b"")
