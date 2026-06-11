import collections

name = "Pandas"
fields = list({"a": 1})
itertuple = collections.namedtuple(name, fields, rename=True)
z = itertuple
