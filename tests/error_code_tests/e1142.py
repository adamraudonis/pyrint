# Test file for error code E1142: await-outside-async
# This file contains code that triggers E1142 ('await' outside async function)

# Example 1: await at module level
import asyncio
await asyncio.sleep(1)  # E1142: await-outside-async

# Example 2: await in regular function
def regular_function():
    await asyncio.sleep(1)  # E1142: await-outside-async

# Example 3: await in class method (non-async)
class MyClass:
    def regular_method(self):
        await asyncio.sleep(1)  # E1142: await-outside-async
    
    async def async_method(self):
        await asyncio.sleep(1)  # OK - inside async method

# Example 4: await in nested regular function inside async
async def outer_async():
    await asyncio.sleep(1)  # OK
    
    def inner_regular():
        await asyncio.sleep(1)  # E1142: await-outside-async
    
    return inner_regular

# Example 5: await in lambda (not allowed)
async def async_with_lambda():
    # Lambda cannot be async, so await is not allowed
    f = lambda x: await x  # E1142: await-outside-async

# Example 6: Valid async function with await
async def valid_async_function():
    result = await asyncio.sleep(1)  # OK
    await asyncio.sleep(2)  # OK
    return result

# Example 7: await in comprehension outside async
result = [await x for x in range(5)]  # E1142: await-outside-async

# Example 8: await in generator expression outside async
gen = (await x for x in range(5))  # E1142: await-outside-async

# Example 9: Nested async functions
async def outer():
    await asyncio.sleep(1)  # OK
    
    async def inner():
        await asyncio.sleep(1)  # OK
    
    return await inner()  # OK