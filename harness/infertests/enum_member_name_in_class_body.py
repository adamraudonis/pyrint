from enum import Enum

class Status(Enum):
    QUEUED = 1
    STARTING = 2
    NON_TERMINAL = (QUEUED, STARTING)

class Set(frozenset, Enum):
    A = {1, 2}
    B = {2, 3}
    UNION = A | B
