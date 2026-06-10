from uuid import uuid4
import uuid

h = uuid4().hex
i = uuid.uuid4().int
a = "%032x" % 0
b = "%-6dx" % 42
c = "%05.1f" % 3.14159
d = "%#x" % 255
e = "%+d" % 7
f = "%.3s" % "abcdef"
