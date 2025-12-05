# Test file for error code E0213: no-self-argument
# This file contains code that triggers E0213 (Method should have "self" as first argument)

# Example 1: Method with wrong first parameter name
class MyClass1:
    def method(this):  # E0213: no-self-argument
        return "wrong parameter name"


# Example 2: Method with generic parameter instead of self
class MyClass2:
    def instance_method(obj):  # E0213: no-self-argument
        return obj
    
    def another_method(instance):  # E0213: no-self-argument
        return instance


# Example 3: Magic methods with wrong first parameter
class MyClass3:
    def __init__(this):  # E0213: no-self-argument
        this.value = 42
    
    def __str__(obj):  # E0213: no-self-argument
        return str(obj.value)
    
    def __repr__(instance):  # E0213: no-self-argument
        return f"MyClass3({instance.value})"


# Example 4: Property methods with wrong first parameter
class MyClass4:
    def __init__(self):
        self._value = 10
    
    @property
    def value(this):  # E0213: no-self-argument
        return this._value
    
    @value.setter
    def value(obj, val):  # E0213: no-self-argument
        obj._value = val


# Example 5: Mixed correct and incorrect parameter names
class MyClass5:
    def correct_method(self):
        return "this is correct"
    
    def incorrect_method(this):  # E0213: no-self-argument
        return "this is incorrect"
    
    def another_incorrect(obj):  # E0213: no-self-argument
        return "also incorrect"


# Example 6: Method with multiple parameters but wrong first one
class MyClass6:
    def method_with_args(this, arg1, arg2):  # E0213: no-self-argument
        return this, arg1, arg2
    
    def method_with_kwargs(obj, *args, **kwargs):  # E0213: no-self-argument
        return obj, args, kwargs


# Example 7: Inherited class with wrong parameter names
class Parent:
    def parent_method(self):
        return "parent"

class Child(Parent):
    def child_method(this):  # E0213: no-self-argument
        return "child"
    
    def override_parent(obj):  # E0213: no-self-argument
        return "overridden"


# Example 8: Special methods with wrong first parameter
class MyClass7:
    def __call__(this):  # E0213: no-self-argument
        return "called"
    
    def __getitem__(obj, key):  # E0213: no-self-argument
        return key
    
    def __setitem__(instance, key, value):  # E0213: no-self-argument
        pass