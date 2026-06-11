class CategoricalDtypeType(type):
    pass


class CategoricalDtype:
    name = "category"
    type: type[CategoricalDtypeType] = CategoricalDtypeType
    kind: str = "O"
