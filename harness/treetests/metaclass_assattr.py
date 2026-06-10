class ModelBase(type):
    def _prepare(cls):
        cls.get_next_in_order = 1
        if cls.__doc__ is None:
            cls.__doc__ = "x"


class RegisterLookupMixin:
    def get_class_lookups(cls):
        return cls.merge_dicts([])

    def register_class_lookup(cls, lookup):
        cls.class_lookups = {}

    get_class_lookups = classmethod(get_class_lookups)
    register_class_lookup = classmethod(register_class_lookup)

    def normal(self):
        self.invisible = 1


class SelfClassAttr:
    def setUp(self):
        self.__class__.databases = {"void"}


class Loader:
    pass


class UsesLoader:
    def setUp(self):
        self.LOADER_CLASS = Loader

    def test(self):
        self.LOADER_CLASS._refresh_file_mapping = 1


def f(flex, opname):
    if flex:
        op = lambda x, y: x + y
        op.__name__ = opname
    else:
        op = getattr(None, opname)
