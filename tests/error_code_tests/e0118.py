# Test file for error code E0118: used-prior-global-declaration
# This file contains code that triggers E0118 (Name used prior to global declaration)

# Example 1: Variable used before global declaration
def function1():
    print(x)  # E0118: used-prior-global-declaration
    global x
    x = 10


# Example 2: Variable modified before global declaration
def function2():
    y = y + 1  # E0118: used-prior-global-declaration
    global y


# Example 3: Multiple variables used before global declaration
def function3():
    result = a + b  # E0118: used-prior-global-declaration
    global a, b
    a = 5
    b = 10


# Example 4: Variable used in conditional before global declaration
def function4():
    if True:
        print(condition_var)  # E0118: used-prior-global-declaration
    global condition_var
    condition_var = True


# Example 5: Variable used in try-except before global declaration
def function5():
    try:
        value = error_var  # E0118: used-prior-global-declaration
    except NameError:
        pass
    global error_var
    error_var = "error"


# Example 6: Variable used in nested function before global declaration
def function6():
    def inner():
        return nested_var  # E0118: used-prior-global-declaration
    
    global nested_var
    nested_var = "nested"
    return inner


# Example 7: Variable used in lambda before global declaration
def function7():
    func = lambda: lambda_var  # E0118: used-prior-global-declaration
    global lambda_var
    lambda_var = "lambda value"
    return func


# Example 8: Variable used in comprehension before global declaration
def function8():
    result = [comp_var for i in range(1)]  # E0118: used-prior-global-declaration
    global comp_var
    comp_var = "comprehension"


# Example 9: Class attribute used before global declaration
class MyClass:
    def method(self):
        return class_var  # E0118: used-prior-global-declaration
    
    global class_var
    class_var = "class level"


# Example 10: Variable used in function call before global declaration
def function9():
    print(f"Value is {call_var}")  # E0118: used-prior-global-declaration
    global call_var
    call_var = "called"