"""
Test file for E0606: possibly-used-before-assignment
"""

# Case 1: Variable only assigned in if branch
def test_conditional_assignment(condition):
    if condition:
        x = 10
    print(x)  # E0606: x might not be assigned

# Case 2: Variable assigned in if but not else
def test_if_else(condition):
    if condition:
        y = 20
    else:
        pass
    return y  # E0606: y might not be assigned

# Case 3: Variable assigned in all branches (no error)
def test_all_branches(condition):
    if condition:
        z = 30
    else:
        z = 40
    return z  # OK: z is assigned in all branches

# Case 4: Variable assigned in nested conditions
def test_nested(a, b):
    if a:
        if b:
            w = 50
    print(w)  # E0606: w might not be assigned

# Case 5: Variable assigned in try but not except
def test_try_except():
    try:
        risky = dangerous_operation()
    except Exception:
        pass
    return risky  # E0606: risky might not be assigned

# Case 6: Variable assigned in loop (might not execute)
def test_loop(items):
    for item in items:
        result = item
    return result  # E0606: result might not be assigned if items is empty

# Case 7: Variable assigned before use (no error)
def test_ok():
    value = 100
    if some_condition():
        value = 200
    return value  # OK: value is definitely assigned

# Case 8: Multiple conditional paths
def test_multiple_conditions(a, b, c):
    if a:
        var = 1
    elif b:
        var = 2
    elif c:
        pass  # Missing assignment here
    else:
        var = 4
    return var  # E0606: var might not be assigned (when c is True)

# Case 9: Variable used in finally
def test_finally():
    try:
        temp = get_value()
    except:
        pass
    finally:
        print(temp)  # E0606: temp might not be assigned

# Case 10: While loop condition
def test_while():
    while condition():
        val = get_next()
    return val  # E0606: val might not be assigned if loop never executes