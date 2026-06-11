class D:
    def __init__(self, **kw):
        pass

tx_dists = [(7000, 22965.83), D(km=7), D(mi=4.349)]
for dist in tx_dists:
    if isinstance(dist, tuple):
        dist1, dist2 = dist
    else:
        dist1 = dist2 = dist
    a = (dist1,)
    b = (dist2,)
