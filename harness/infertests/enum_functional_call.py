from enum import Enum

Cols = Enum("Cols", "col1 col2")
a = Cols.col1
b = Cols
c = a.name
d = a.value
