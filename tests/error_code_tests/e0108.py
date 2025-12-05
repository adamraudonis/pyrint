# Test file for error code E0108: duplicate-argument-name
# This file contains code that triggers E0108 (Duplicate argument name in function definition)

# Example 1: Function with duplicate parameter names
def function1(x, y, x):  # E0108: duplicate-argument-name
    return x + y


# Example 2: Function with multiple duplicate parameters
def function2(a, b, c, a, b):  # E0108: duplicate-argument-name
    return a + b + c


# Example 3: Function with positional and keyword duplicate
def function3(name, age, name="default"):  # E0108: duplicate-argument-name
    return f"{name} is {age} years old"


# Example 4: Method with duplicate parameters
class MyClass1:
    def method(self, param1, param2, param1):  # E0108: duplicate-argument-name
        return param1 + param2


# Example 5: Function with *args and duplicate name
def function4(x, *args, x):  # E0108: duplicate-argument-name
    return x


# Example 6: Function with **kwargs and duplicate name  
def function5(name, **kwargs, name):  # E0108: duplicate-argument-name
    return name


# Example 7: Complex function with multiple types of duplicates
def function6(a, b, *args, a, **kwargs):  # E0108: duplicate-argument-name
    return a + b


# Example 8: Static method with duplicate parameters
class MyClass2:
    @staticmethod
    def static_method(x, y, z, x):  # E0108: duplicate-argument-name
        return x * y * z


# Example 9: Class method with duplicate parameters
class MyClass3:
    @classmethod
    def class_method(cls, param, other, param):  # E0108: duplicate-argument-name
        return param