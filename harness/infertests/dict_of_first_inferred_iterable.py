from urllib.parse import parse_qsl


def f(q):
    query = dict(parse_qsl(q))
    query
