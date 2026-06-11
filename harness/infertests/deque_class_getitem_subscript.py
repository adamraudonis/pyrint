from collections import deque


class FilterState:
    timestamp = 0.0


class TimeSMAFilter:
    def __init__(self) -> None:
        self.queue = deque[FilterState]()

    def f(self):
        x = self.queue
        y = self.queue.popleft()
        self.queue.append(1)
        return x, y
