from socket import error as socket_error
MSG = "\n\n    Unable to send"
def f():
    try:
        g()
    except socket_error as e:
        return e.strerror + "\n" + MSG
    except ImportError as e2:
        return e2.name
    return None
x = f()
x
