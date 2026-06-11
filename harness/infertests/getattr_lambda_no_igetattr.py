class MW:
    rfn = "next"
    def get(self, view_func):
        return getattr(view_func, "rfn", self.rfn)

m = MW()
a = m.get(lambda: None)
a
b = getattr(lambda: None, "x", 5)
b
c = getattr(MW, "rfn", 7)
c
d = getattr([1, 2], "nope", 9)
d
