import re
version_component_re = re.compile(r"(\d+|[a-z]+|\.)")

def get_version_tuple(version):
    version_numbers = []
    for item in version_component_re.split(version):
        if item and item != ".":
            try:
                component = int(item)
            except ValueError:
                break
            else:
                version_numbers.append(component)
    return tuple(version_numbers)

def pv():
    return get_version_tuple("x")

a = pv() >= (3, 2)
a
b = () >= (3, 2)
b
c = (1, 2) >= (1,)
c
d = (1, "x") >= (1, 2)
d
e = (1, 2) < (1, 3)
e
f = [1, 2] <= [1, 2]
f
g = (1, 2) < [1, 3]
g
h = {1, 2} <= {1, 2, 3}
h
i = {1, 4} < {1, 2, 3}
i
j = ((1, 2), 3) >= ((1, 1), 9)
j
k = ("a", "b") > ("a",)
k
