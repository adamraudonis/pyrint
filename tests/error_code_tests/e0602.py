"""
Test file for E0602: undefined-variable
"""

# Case 1: Undefined variable at module level
print(undefined_module_var)  # E0602: Undefined variable

# Case 2: Undefined variable in function
def test_undefined_in_function():
    result = unknown_var + 10  # E0602: Undefined variable
    return result

# Case 3: Undefined variable in method
class TestClass:
    def method(self):
        value = missing_var * 2  # E0602: Undefined variable
        return value

# Case 4: Undefined function call
def test_undefined_function():
    result = undefined_func()  # E0602: Undefined function
    return result

# Case 5: Variable defined after use
def test_use_before_definition():
    print(later_var)  # E0602: Undefined variable (use before definition)
    later_var = 5

# Case 6: Typo in variable name
def test_typo():
    correct_name = 10
    return correct_nane  # E0602: Undefined variable (typo)

# Case 7: Undefined in conditional
def test_conditional():
    if some_undefined_condition:  # E0602: Undefined variable
        return True
    return False

# Case 8: Undefined in list comprehension
def test_list_comp():
    return [undefined_item for undefined_item in unknown_list]  # E0602: unknown_list is undefined

# Case 9: Valid imported names (no error)
from os import path
import sys

def test_valid_imports():
    print(path.exists('.'))  # OK: path is imported
    print(sys.version)  # OK: sys is imported

# Case 10: Valid function and class definitions (no error)
def defined_function():
    return 42

class DefinedClass:
    pass

def test_valid_definitions():
    result = defined_function()  # OK: defined_function is defined above
    obj = DefinedClass()  # OK: DefinedClass is defined above
    return result, obj

# Case 11: Valid builtins (no error)
def test_builtins():
    print(len([1, 2, 3]))  # OK: len is a builtin
    result = str(123)  # OK: str is a builtin
    return isinstance(result, str)  # OK: isinstance is a builtin

# Case 12: Global and nonlocal (no error)
global_var = 100

def test_global():
    global global_var
    return global_var  # OK: global_var is declared global

def test_nonlocal():
    outer_var = 200
    
    def inner():
        nonlocal outer_var
        return outer_var  # OK: outer_var is declared nonlocal
    
    return inner()

# Case 13: Variables defined in function scope (no error)
def test_local_vars():
    local_var = 300
    another_local = local_var + 100  # OK: local_var is defined
    return another_local

# Case 14: Function parameters (no error)
def test_parameters(param1, param2):
    return param1 + param2  # OK: param1 and param2 are parameters

# Case 15: Loop variables (no error)
def test_loop_vars():
    for i in range(10):
        print(i)  # OK: i is the loop variable
    
    result = []
    for item in [1, 2, 3]:
        result.append(item)  # OK: item is the loop variable
    return result

# Case 16: Exception variables (no error)
def test_exception():
    try:
        risky_operation()
    except Exception as e:
        print(e)  # OK: e is the exception variable

# Case 17: With statement variables (no error)
def test_with():
    with open('file.txt') as f:
        return f.read()  # OK: f is defined by the with statement

# Case 18: Module level assignments (no error)
MODULE_CONSTANT = 'constant'

def test_module_constant():
    return MODULE_CONSTANT  # OK: MODULE_CONSTANT is defined at module level

# Case 19: Class attributes accessed incorrectly
class MyClass:
    class_attr = 100
    
    def method(self):
        return class_attr  # E0602: Should be self.class_attr or MyClass.class_attr

# Case 20: Missing self in method
class AnotherClass:
    def __init__(self):
        self.instance_var = 200
    
    def bad_method(self):
        return instance_var  # E0602: Should be self.instance_var