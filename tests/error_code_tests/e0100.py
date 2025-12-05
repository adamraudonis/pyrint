# Test file for error code E0100: init-is-generator
# This file contains code that triggers E0100 (__init__ method is defined as a generator)

# Example 1: Class with __init__ method that yields
class MyClass1:
    def __init__(self):
        yield 1  # E0100: __init__ method is defined as a generator


# Example 2: Class with __init__ method containing yield from
class MyClass2:
    def __init__(self, data):
        yield from data  # E0100: __init__ method is defined as a generator


# Example 3: More complex __init__ with yield in conditional
class MyClass3:
    def __init__(self, condition):
        self.condition = condition
        if condition:
            yield "initialized"  # E0100: __init__ method is defined as a generator


# Example 4: __init__ with multiple yields
class MyClass4:
    def __init__(self, start, end):
        for i in range(start, end):
            yield i  # E0100: __init__ method is defined as a generator


# Example 5: __init__ with yield in try-except block
class MyClass5:
    def __init__(self, value):
        try:
            yield value  # E0100: __init__ method is defined as a generator
        except Exception:
            pass