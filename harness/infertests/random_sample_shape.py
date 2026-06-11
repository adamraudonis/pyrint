import random

subs = ["a", "b", "c", "d", "e", "f"]


def f():
    for m in random.sample(subs, 4):
        print(m)
    x = random.sample(subs, 4)
    return x
