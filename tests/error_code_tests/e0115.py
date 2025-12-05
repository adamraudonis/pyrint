# Test file for error code E0115: nonlocal-and-global
# This file contains code that triggers E0115 (Name is both nonlocal and global)

# Example 1: Variable declared as both global and nonlocal
x = "module level"

def outer_function1():
    x = "outer level"
    
    def inner_function():
        global x  # E0115: nonlocal-and-global
        nonlocal x  # E0115: nonlocal-and-global
        x = "inner level"
    
    return inner_function


# Example 2: Multiple variables with conflicting declarations
y = "global y"
z = "global z"

def outer_function2():
    y = "outer y"
    z = "outer z"
    
    def inner_function():
        global y, z  # E0115: nonlocal-and-global
        nonlocal y, z  # E0115: nonlocal-and-global
        y = "modified y"
        z = "modified z"
    
    return inner_function


# Example 3: Mixed declarations in same function
counter = 0

def outer_function3():
    counter = 10
    
    def inner_function():
        global counter  # E0115: nonlocal-and-global
        nonlocal counter  # E0115: nonlocal-and-global
        counter += 1
        return counter
    
    return inner_function


# Example 4: Declarations in different order
data = []

def outer_function4():
    data = [1, 2, 3]
    
    def inner_function():
        nonlocal data  # E0115: nonlocal-and-global
        global data    # E0115: nonlocal-and-global
        data.append(4)
    
    return inner_function


# Example 5: Class method with conflicting declarations
class MyClass:
    class_var = "class level"
    
    def outer_method(self):
        class_var = "method level"
        
        def inner_function():
            global class_var   # E0115: nonlocal-and-global
            nonlocal class_var  # E0115: nonlocal-and-global
            class_var = "inner level"
        
        return inner_function