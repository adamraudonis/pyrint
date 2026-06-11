class _NoopSpan:
    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        return False


def _start_with_context(name):
    yield None


def f(flag, name):
    if flag:
        span = _NoopSpan()
    else:
        span = _start_with_context(name)
    span.__enter__()
    ok = span.__exit__(None, None, None)
    return ok
