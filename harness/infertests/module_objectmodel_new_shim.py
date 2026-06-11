import ctypes

class ContiguousUnicode(ctypes.Structure):
    @classmethod
    def from_address_copy(cls, address, size=None):
        x = ctypes.Structure.__new__(cls)
        use(x)
        return x
