def f(*args):
    x = f"IN {args}"
    return x

def g(*args, **kwargs):
    return "salt://{}".format(args)
