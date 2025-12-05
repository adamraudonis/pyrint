# Test file for error code E0106: return-arg-in-generator
# This file contains code that triggers E0106 (Return statement with argument inside a generator)

# Example 1: Simple generator with return statement containing value
def generator1():
    yield 1
    yield 2
    return "done"  # E0106: return-arg-in-generator


# Example 2: Generator with conditional return with argument
def generator2(condition):
    for i in range(5):
        yield i
        if condition and i > 2:
            return "early exit"  # E0106: return-arg-in-generator


# Example 3: Generator with multiple return statements with arguments
def generator3():
    yield "start"
    if True:
        return "first return"  # E0106: return-arg-in-generator
    else:
        return "second return"  # E0106: return-arg-in-generator


# Example 4: Generator with return in try-except block
def generator4():
    try:
        yield 1
        yield 2
        return "success"  # E0106: return-arg-in-generator
    except Exception:
        return "error"  # E0106: return-arg-in-generator


# Example 5: Generator with return in nested structure
def generator5():
    for i in range(3):
        yield i
        if i == 2:
            for j in range(2):
                if j == 1:
                    return "nested return"  # E0106: return-arg-in-generator


# Example 6: Generator method in class with return argument
class MyClass:
    def generator_method(self):
        yield self
        return "method done"  # E0106: return-arg-in-generator


# Example 7: Async generator with return argument
async def async_generator():
    yield 1
    yield 2
    return "async done"  # E0106: return-arg-in-generator