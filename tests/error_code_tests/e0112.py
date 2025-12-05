# Test file for error code E0112: too-many-star-expressions
# This file contains code that triggers E0112 (More than one starred expression in assignment)

# Example 1: Multiple starred expressions in tuple assignment
a, *b, *c = [1, 2, 3, 4, 5]  # E0112: too-many-star-expressions


# Example 2: Multiple starred expressions in list assignment
[x, *y, *z] = [1, 2, 3, 4]  # E0112: too-many-star-expressions


# Example 3: Multiple starred expressions in function call context
def function1(*args, **kwargs):
    pass

# This would be in assignment context:
*first, *second = [1, 2, 3]  # E0112: too-many-star-expressions


# Example 4: Multiple starred expressions with mixed variables
start, *middle1, *middle2, end = range(10)  # E0112: too-many-star-expressions


# Example 5: Nested structures with multiple starred expressions
((a, *b), *c) = [[1, 2, 3], 4, 5]  # E0112: too-many-star-expressions


# Example 6: Multiple starred expressions in for loop
for *x, *y in [[1, 2, 3], [4, 5, 6]]:  # E0112: too-many-star-expressions
    pass


# Example 7: Multiple starred expressions in comprehension
result = [item for *x, *y in [[1, 2], [3, 4]]]  # E0112: too-many-star-expressions


# Example 8: Multiple starred expressions with function return
def get_data():
    return [1, 2, 3, 4, 5]

*first_part, *second_part = get_data()  # E0112: too-many-star-expressions


# Example 9: Multiple starred expressions in exception handling context
try:
    raise ValueError("test")
except ValueError as e:
    *error1, *error2 = str(e).split()  # E0112: too-many-star-expressions