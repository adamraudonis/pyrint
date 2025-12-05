# Test file for error code E0117: nonlocal-without-binding
# This file contains code that triggers E0117 (Nonlocal name found without binding)

# Example 1: nonlocal variable not defined in enclosing scope
def function1():
    def inner_function():
        nonlocal x  # E0117: nonlocal-without-binding
        x = 10
    return inner_function


# Example 2: nonlocal variable only exists at global level
global_var = "I'm global"

def function2():
    def inner_function():
        nonlocal global_var  # E0117: nonlocal-without-binding
        global_var = "modified"
    return inner_function


# Example 3: Multiple nonlocal variables, some without binding
def function3():
    y = "exists in enclosing scope"
    
    def inner_function():
        nonlocal y      # This is OK
        nonlocal z      # E0117: nonlocal-without-binding
        y = "modified"
        z = "new value"
    
    return inner_function


# Example 4: nonlocal in nested function with no intermediate scope
def function4():
    def middle_function():
        def inner_function():
            nonlocal undefined_var  # E0117: nonlocal-without-binding
            undefined_var = "value"
        return inner_function
    return middle_function


# Example 5: nonlocal variable defined after nonlocal declaration
def function5():
    def inner_function():
        nonlocal later_defined  # E0117: nonlocal-without-binding
        later_defined = "value"
    
    # This won't help - it's defined after the inner function
    later_defined = "too late"
    return inner_function


# Example 6: Class method with nonlocal without binding
class MyClass:
    def outer_method(self):
        def inner_function():
            nonlocal self  # E0117: nonlocal-without-binding (self is parameter, not local)
            pass
        return inner_function


# Example 7: nonlocal in lambda function
def function6():
    return lambda: (lambda: [nonlocal missing_var, None][1])()  # E0117: nonlocal-without-binding


# Example 8: Multiple levels with missing binding
def function7():
    def level1():
        def level2():
            nonlocal deep_var  # E0117: nonlocal-without-binding
            deep_var = "value"
        return level2
    return level1