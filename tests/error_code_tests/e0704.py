# Test file for error code E0704: misplaced-bare-raise
# This file contains code that triggers E0704 (bare raise not in except clause)

# Example 1: Bare raise at module level
raise  # E0704: misplaced-bare-raise

# Example 2: Bare raise in function
def function1():
    raise  # E0704: misplaced-bare-raise

# Example 3: Bare raise in if statement
def function2():
    if True:
        raise  # E0704: misplaced-bare-raise

# Example 4: Bare raise in for loop
def function3():
    for i in range(5):
        raise  # E0704: misplaced-bare-raise

# Valid: Bare raise in except handler
def function4():
    try:
        x = 1 / 0
    except:
        raise  # OK - in except handler

# Valid: Bare raise in nested except
def function5():
    try:
        dangerous_operation()
    except Exception:
        try:
            cleanup()
        except:
            raise  # OK - in except handler
        raise  # OK - still in except handler

# Example 5: Bare raise after except block
def function6():
    try:
        x = 1 / 0
    except:
        pass
    raise  # E0704: misplaced-bare-raise

# Example 6: Bare raise in finally
def function7():
    try:
        x = 1 / 0
    except:
        pass
    finally:
        raise  # E0704: misplaced-bare-raise

# Valid: Non-bare raise anywhere
def function8():
    raise ValueError("This is fine")  # OK - not a bare raise

# Example 7: Bare raise in else clause of try
def function9():
    try:
        x = 1
    except:
        pass
    else:
        raise  # E0704: misplaced-bare-raise