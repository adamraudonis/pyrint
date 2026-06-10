class A:
    def __init__(self):
        self.x = 1
        self.y = "s"
        self.x = 2

    def m(self):
        state = self.__dict__.copy()
        state
        d2 = self.__dict__
        d2["x"]


GLOBAL_ONE = 1
GLOBAL_ONE = 2


def mod_dict():
    import probe
    g = probe.__dict__
    g


def mod_dict2():
    import probe
    probe.__dict__["GLOBAL_ONE"]
