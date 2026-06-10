import sys
from contextlib import contextmanager
from io import StringIO


@contextmanager
def captured_output(stream_name):
    orig_stdout = getattr(sys, stream_name)
    setattr(sys, stream_name, StringIO())
    try:
        yield getattr(sys, stream_name)
    finally:
        setattr(sys, stream_name, orig_stdout)


def captured_stderr():
    return captured_output("stderr")


def use():
    with captured_stderr() as stderr:
        print("x", file=sys.stderr)
    return stderr.getvalue()


def use2():
    with captured_output("stdout") as out:
        pass
    return out.getvalue()
