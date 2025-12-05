# Test file for error code E0107: nonexistent-operator
# This file contains code that triggers E0107 (Use of the non-existent operator)

# Example 1: Using the old <> operator (removed in Python 3)
if 5 <> 10:  # E0107: nonexistent-operator
    pass

# Example 2: Using the old <> operator in expression
x = (3 <> 4)  # E0107: nonexistent-operator

# Example 3: Valid operators should not trigger errors
if 5 != 10:  # OK
    pass

if 5 == 10:  # OK
    pass

if 5 < 10:  # OK
    pass

if 5 > 10:  # OK
    pass

if 5 <= 10:  # OK
    pass

if 5 >= 10:  # OK
    pass