class Base:
    def __enter__(self):
        return self
    def __exit__(self, *a):
        return False


class Sub(Base):
    pass


def use():
    with Sub() as s:
        pass
    return s
