"""Test cases for Pyrint - Python code samples to test error detection"""

# E0001: syntax-error
syntax_error_code = '''
def bad_function(
    print("missing closing paren"
'''

# E0100: init-is-generator
init_generator_code = '''
class MyClass:
    def __init__(self):
        yield 1
'''

# E0101: return-in-init
return_in_init_code = '''
class MyClass:
    def __init__(self):
        return "value"
'''

# E0102: function-redefined
function_redefined_code = '''
def my_func():
    pass

def my_func():
    pass
'''

# E0103: not-in-loop
not_in_loop_code = '''
def test():
    break
    
continue
'''

# E0104: return-outside-function
return_outside_function_code = '''
return 42
'''

# E0105: yield-outside-function
yield_outside_function_code = '''
yield 1
'''

# E0106: return-arg-in-generator
return_in_generator_code = '''
def my_generator():
    yield 1
    return 2
'''

# E0108: duplicate-argument-name
duplicate_arg_code = '''
def func(arg1, arg1):
    pass
'''

# E0112: too-many-star-expressions
too_many_stars_code = '''
*a, *b = [1, 2, 3]
'''

# E0114: star-needs-assignment-target
star_needs_assignment_code = '''
def func():
    return *[1, 2, 3]
'''

# E0115: nonlocal-and-global
nonlocal_and_global_code = '''
def outer():
    x = 1
    def inner():
        global x
        nonlocal x
'''

# E0116: continue-not-in-loop
continue_not_in_loop_code = '''
def test():
    continue
'''

# E0117: nonlocal-without-binding
nonlocal_without_binding_code = '''
def func():
    nonlocal x
'''

# E0118: used-prior-global-declaration
used_prior_global_code = '''
def func():
    x = 10
    global x
'''

# Valid code that should not produce errors
valid_code = '''
class MyClass:
    def __init__(self):
        self.value = 10
        
    def method(self):
        return self.value

def normal_function(arg1, arg2):
    for i in range(10):
        if i == 5:
            break
        if i == 3:
            continue
    return arg1 + arg2

def generator_function():
    yield 1
    yield 2
    
x = 10

def uses_global():
    global x
    x = 20
'''