import asyncio


async def f(work):
    try:
        async with asyncio.TaskGroup() as tg:
            tg.create_task(work())
    except ExceptionGroup as eg:
        raise eg.exceptions[0]


def g():
    try:
        pass
    except ExceptionGroup as eg:
        x = eg.exceptions
        return x
