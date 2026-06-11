class MetaClass:
    """Look some methods in the implicit metaclass."""

    @classmethod
    def whatever(cls):
        return cls.mro() + cls.missing()
