class A:
    def f(self):
        self.__doc__ = "x"
        y = self.__doc__
        y

class M(type):
    def f(cls):
        cls.__doc__ = "y"
        z = cls.__doc__
        z
