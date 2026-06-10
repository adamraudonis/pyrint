def in_ipython_frontend():
    try:
        ip = get_ipython()
        return "zmq" in str(type(ip)).lower()
    except NameError:
        pass
    return False

x = in_ipython_frontend()
x
