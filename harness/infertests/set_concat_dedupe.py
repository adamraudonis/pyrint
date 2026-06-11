from urllib.parse import uses_netloc, uses_params, uses_relative

_VALID_URLS = set(uses_relative + uses_netloc + uses_params)
_VALID_URLS.discard("")
print(_VALID_URLS)

L = maybe_mangle_lambdas(lambda x: x).__name__


def maybe_mangle_lambdas(agg_spec):
    return agg_spec
