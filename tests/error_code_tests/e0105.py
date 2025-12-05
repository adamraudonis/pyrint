# Test file for error code E0105: yield-outside-function
# This file contains code that triggers E0105 (Yield statement outside of a function)

# Example 1: yield at module level
yield 1  # E0105: yield-outside-function


# Example 2: yield from at module level  
data = [1, 2, 3]
yield from data  # E0105: yield-outside-function


# Example 3: yield in if statement at module level
if True:
    yield "hello"  # E0105: yield-outside-function


# Example 4: yield in try-except at module level
try:
    yield 42  # E0105: yield-outside-function
except Exception:
    pass


# Example 5: yield in for loop at module level
for i in range(3):
    yield i  # E0105: yield-outside-function


# Example 6: yield in while loop at module level
x = 0
while x < 3:
    yield x  # E0105: yield-outside-function
    x += 1


# Example 7: yield in class definition (outside method)
class MyClass:
    yield "class level"  # E0105: yield-outside-function
    
    def proper_method(self):
        yield "this is OK"  # This should not trigger E0105


# Example 8: yield in nested structure at module level
if True:
    for i in range(2):
        if i > 0:
            yield i * 2  # E0105: yield-outside-function