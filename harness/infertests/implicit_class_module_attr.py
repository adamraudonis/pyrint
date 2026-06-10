class Base:
    def m(self):
        cls = self.__class__
        a = cls.__module__
        b = cls.__name__
        return a
