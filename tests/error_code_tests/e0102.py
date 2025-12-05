# Test file for error code E0102: function-redefined
# This file contains code that triggers E0102 (Function or method already defined with same name)

# Example 1: Function defined twice with same name
def my_function():
    return "first definition"

def my_function():  # E0102: function-redefined
    return "second definition"


# Example 2: Method redefined in same class
class MyClass1:
    def process(self):
        return "first method"
    
    def process(self):  # E0102: function-redefined
        return "second method"


# Example 3: Function redefined after initial definition
def calculate(x):
    return x * 2

def calculate(x, y):  # E0102: function-redefined
    return x * y


# Example 4: Multiple redefinitions
def helper():
    pass

def helper():  # E0102: function-redefined
    return 1

def helper():  # E0102: function-redefined
    return 2


# Example 5: Class methods redefined
class MyClass2:
    def __init__(self):
        pass
    
    def __init__(self, value):  # E0102: function-redefined
        self.value = value
    
    def method(self):
        return "original"
    
    def method(self):  # E0102: function-redefined
        return "redefined"