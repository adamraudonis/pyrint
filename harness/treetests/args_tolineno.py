def multi(
    a,
    b=1,
):
    pass


def kwonly(*,
           c=(1,
              2)
           ):
    pass


def va(
    *args: (str)
):
    pass


def kw(
    **kwargs: (bytes)
):
    pass


def plain(*args, **kwargs):
    pass


f = lambda: 0
g = lambda x=1: x
