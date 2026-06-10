class BaseDumper:
    def __init_subclass__(cls, base):
        cls.GeometryDumper = base
        cls.other = 1

    @classmethod
    def make(cls):
        cls.from_make = 2

    def method(self):
        self.invisible = 3

    @staticmethod
    def st(cls):
        cls.not_added = 4

    def __new__(cls):
        cls.from_new = 5
