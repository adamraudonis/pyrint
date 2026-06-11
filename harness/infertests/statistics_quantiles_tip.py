import statistics


def f(states, percentile):
    if len(states) > 1:
        v = statistics.quantiles(states, n=100, method="exclusive")[percentile - 1]
    else:
        v = None
    return v
