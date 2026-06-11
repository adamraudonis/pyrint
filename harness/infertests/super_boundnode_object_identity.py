class Desc:
    f: float | None = None


class Base:
    @property
    def v(self) -> float:
        return 100.0


class Sub(Base):
    d: Desc

    @property
    def v(self) -> float:
        return self.d.f if self.d.f else super().v

    @property
    def w(self) -> float:
        return self.d.f if self.d.f else super().v
