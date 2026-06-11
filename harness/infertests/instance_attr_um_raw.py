class StreamOutput:
    def recv(self):
        return False

    def part_recv(self):
        return True


class HLSSync:
    def __init__(self):
        self._original_recv = StreamOutput.recv

    async def recv(self):
        return await self._original_recv(self)

    def f(self):
        r = self._original_recv(self)
        return r
