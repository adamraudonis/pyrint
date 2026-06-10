class BufferedReader:
    def read(self):
        return 1


class TextIOWrapper:
    pass


class TokenBase:
    pass


def infix(bp):
    class Operator(TokenBase):
        lbp = bp

    return Operator


def prefix(bp):
    class Operator(TokenBase):
        lbp = bp

    return Operator


OPERATORS = {
    "or": infix(6),
    "not": prefix(8),
    "base": TokenBase,
}
for key, op in OPERATORS.items():
    op.id = key
