class Meta(type):
    def __new__(mcs, name, bases, attrs):
        new_class = super().__new__(mcs, name, bases, attrs)
        return new_class
class Base(metaclass=Meta):
    pass
def factory(name, base):
    return type(base)(name + "X", (base,), {"fld": 1})
F = factory("My", Base)
F
F.fld
t = type.__new__(type, "T", (object,), {"z": 9})
t
t.z
