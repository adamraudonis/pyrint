class GeometryCollection:
    _typeid = 7

    def kml(self):
        return "x"


GeometryCollection._allowed = (1, 2)
GeometryCollection.kml2 = GeometryCollection.kml


def attach():
    GeometryCollection.from_func = 1


obj = GeometryCollection()
obj.not_in_locals = 3
