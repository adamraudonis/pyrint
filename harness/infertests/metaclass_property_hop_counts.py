class Meta(type):
    @property
    def choices(cls):
        empty = [(None, cls.__empty__)] if hasattr(cls, "__empty__") else []
        return empty + [(m.value, m.label) for m in cls]

class Suit(metaclass=Meta):
    DIAMOND = 1

x = Suit.choices
x
y = len(None)
y
z = len(5)
z
