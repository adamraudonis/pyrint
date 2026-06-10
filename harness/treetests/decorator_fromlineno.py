import functools


@functools.lru_cache(maxsize=2)
# comment line one
# comment line two
def gap(self, jwt):
    return jwt


@property
def adjacent(self):
    return 1


@functools.wraps(
    gap
)
def spans(x):
    return x


@property
# comment
async def agap(self):
    return 2
