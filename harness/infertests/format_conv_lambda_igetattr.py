cluster_option_name = "cluster"
cluster_constructor_option_names = frozenset(("hosts",))


def check(options):
    if cluster_option_name in options:
        raise ValueError(
            "Cannot provide both named cluster ({!r}) and cluster configuration ({}) options.".format(
                cluster_option_name,
                ", ".join(repr(name) for name in cluster_constructor_option_names),
            )
        )


def maybe_mangle_lambdas(agg_spec):
    return agg_spec


x = maybe_mangle_lambdas(lambda x: x).__name__
