import asyncio

async def go(stop_event, hass):
    pending_info: dict[tuple[str, str], asyncio.Task] = {}
    async for domain, domain_data in something(hass):
        for key, value in domain_data["info"].items():
            pending_info[(domain, key)] = value
    tasks: set[asyncio.Task] = {
        asyncio.create_task(stop_event.wait()),
        *pending_info.values(),
    }
    while len(tasks) > 1 and not stop_event.is_set():
        done, tasks = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        if stop_event.is_set():
            for task in tasks:
                task.cancel()
