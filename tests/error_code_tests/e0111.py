# Test file for error code E0111: bad-reversed-sequence
# This file contains code that triggers E0111 (The first reversed() argument is not a sequence)

# Example 1: Reversing a number (not a sequence)
x = reversed(123)  # E0111: bad-reversed-sequence

# Example 2: Reversing None
y = reversed(None)  # E0111: bad-reversed-sequence

# Example 3: Reversing a boolean
z = reversed(True)  # E0111: bad-reversed-sequence

# Example 4: Reversing a function
def my_func():
    pass
w = reversed(my_func)  # E0111: bad-reversed-sequence

# Valid examples (should not trigger error)
a = reversed([1, 2, 3])  # OK - list is a sequence
b = reversed((1, 2, 3))  # OK - tuple is a sequence
c = reversed("hello")  # OK - string is a sequence
d = reversed(range(10))  # OK - range is a sequence