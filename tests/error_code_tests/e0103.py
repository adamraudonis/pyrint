# Test file for error code E0103: not-in-loop
# This file contains code that triggers E0103 (break/continue statement not properly in a loop)

# Example 1: break statement outside of loop
def function1():
    if True:
        break  # E0103: not-in-loop


# Example 2: continue statement outside of loop
def function2():
    if True:
        continue  # E0103: not-in-loop


# Example 3: break in function but not in loop
def function3():
    x = 5
    if x > 3:
        break  # E0103: not-in-loop


# Example 4: continue in function but not in loop
def function4():
    y = 10
    if y < 20:
        continue  # E0103: not-in-loop


# Example 5: break in nested function outside loop
def outer_function():
    def inner_function():
        break  # E0103: not-in-loop
    return inner_function


# Example 6: continue in nested function outside loop
def another_outer():
    def another_inner():
        continue  # E0103: not-in-loop
    return another_inner


# Example 7: break in try-except block outside loop
def function5():
    try:
        x = 1 / 0
    except ZeroDivisionError:
        break  # E0103: not-in-loop


# Example 8: continue in class method outside loop
class MyClass:
    def method(self):
        if self:
            continue  # E0103: not-in-loop