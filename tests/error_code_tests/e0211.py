# Test file for error code E0211: no-method-argument
# This file contains code that triggers E0211 (Method has no argument - missing self)

# Example 1: Instance method without any parameters
class MyClass1:
    def method():  # E0211: no-method-argument
        return "no arguments"


# Example 2: Multiple methods without parameters
class MyClass2:
    def first_method():  # E0211: no-method-argument
        pass
    
    def second_method():  # E0211: no-method-argument
        return 42


# Example 3: Method that should take self but doesn't
class MyClass3:
    def __init__():  # E0211: no-method-argument
        pass
    
    def instance_method():  # E0211: no-method-argument
        return "instance"


# Example 4: Property method without self
class MyClass4:
    @property
    def my_property():  # E0211: no-method-argument
        return "property value"


# Example 5: Method with decorators but no arguments
class MyClass5:
    @classmethod
    def class_method():  # This might be OK as classmethod should have cls
        pass
    
    def regular_method():  # E0211: no-method-argument
        pass


# Example 6: Magic method without self
class MyClass6:
    def __str__():  # E0211: no-method-argument
        return "string representation"
    
    def __len__():  # E0211: no-method-argument
        return 0


# Example 7: Method that tries to access instance attributes without self
class MyClass7:
    def __init__(self):
        self.value = 42
    
    def get_value():  # E0211: no-method-argument
        # This would cause an error at runtime too
        return self.value  # NameError: name 'self' is not defined


# Example 8: Multiple inheritance with methods missing self
class Parent:
    def parent_method(self):
        return "parent"

class Child(Parent):
    def child_method():  # E0211: no-method-argument
        return "child"
    
    def override_method():  # E0211: no-method-argument
        return "overridden"