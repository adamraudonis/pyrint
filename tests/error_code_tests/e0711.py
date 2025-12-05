# Test file for error code E0711: notimplemented-raised
# This file contains code that triggers E0711 (NotImplemented raised instead of NotImplementedError)

# Example 1: Raising NotImplemented directly
def method1():
    raise NotImplemented  # E0711: notimplemented-raised

# Example 2: Raising NotImplemented in class method
class MyClass:
    def unfinished_method(self):
        raise NotImplemented  # E0711: notimplemented-raised

# Example 3: Raising NotImplemented in exception handler
def method2():
    try:
        x = 1 / 0
    except:
        raise NotImplemented  # E0711: notimplemented-raised

# Example 4: Raising NotImplemented conditionally
def method3(condition):
    if condition:
        raise NotImplemented  # E0711: notimplemented-raised
    return "done"

# Valid: Raising NotImplementedError (correct usage)
def correct_method():
    raise NotImplementedError  # OK

# Valid: Returning NotImplemented (for operator overloading)
class MyClass2:
    def __add__(self, other):
        return NotImplemented  # OK - returning, not raising

# Example 5: Raising NotImplemented in property
class MyClass3:
    @property
    def my_property(self):
        raise NotImplemented  # E0711: notimplemented-raised