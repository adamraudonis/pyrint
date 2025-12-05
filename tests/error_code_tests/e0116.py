# Test file for error code E0116: continue-not-in-loop
# This file contains code that triggers E0116 ('continue' statement not properly in a loop)

# Example 1: continue statement outside of any loop
def function1():
    if True:
        continue  # E0116: continue-not-in-loop


# Example 2: continue statement in function but not in loop
def function2():
    x = 5
    if x > 3:
        continue  # E0116: continue-not-in-loop


# Example 3: continue statement in nested function outside loop
def outer_function():
    def inner_function():
        continue  # E0116: continue-not-in-loop
    return inner_function


# Example 4: continue statement in try-except block outside loop
def function3():
    try:
        x = 1 / 1
    except ZeroDivisionError:
        continue  # E0116: continue-not-in-loop


# Example 5: continue statement in class method outside loop
class MyClass1:
    def method(self):
        if self:
            continue  # E0116: continue-not-in-loop


# Example 6: continue statement in if-else outside loop
def function4():
    condition = True
    if condition:
        continue  # E0116: continue-not-in-loop
    else:
        pass


# Example 7: continue statement in with block outside loop
def function5():
    with open(__file__) as f:
        continue  # E0116: continue-not-in-loop


# Example 8: continue statement in lambda (though this would be syntax error)
# lambda: continue  # This would be a syntax error, but showing concept

# Example 9: continue statement after loop ends
def function6():
    for i in range(3):
        pass
    continue  # E0116: continue-not-in-loop


# Example 10: continue statement in finally block outside loop
def function7():
    try:
        pass
    finally:
        continue  # E0116: continue-not-in-loop