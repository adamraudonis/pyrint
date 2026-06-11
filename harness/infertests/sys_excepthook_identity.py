# sys.excepthook and sys.__excepthook__ are DISTINCT raw-built FunctionDefs
# in astroid (one fresh node per member name) even though they wrap the same
# C function — gen_snapshot Ref-dedup must NOT merge them. Conversely
# builtins.type is ONE object shared across every exception's __class__.
import sys

original = sys.excepthook
backup = sys.__excepthook__

def spy(a, b, c):
    pass

sys.excepthook = spy
x = sys.excepthook
y = sys.__excepthook__
