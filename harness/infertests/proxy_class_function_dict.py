import types


def namespaced_function(function, global_dict):
    new_namespaced_function = types.FunctionType(
        function.__code__,
        global_dict,
        name=function.__name__,
        argdefs=function.__defaults__,
        closure=function.__closure__,
    )
    new_namespaced_function.__dict__.update(function.__dict__)
    return new_namespaced_function


def alias_function(fun, name, doc=None):
    alias_fun = types.FunctionType(
        fun.__code__,
        fun.__globals__,
        str(name),
        fun.__defaults__,
        fun.__closure__,
    )
    alias_fun.__dict__.update(fun.__dict__)
    alias_fun.__doc__ = doc
    return alias_fun
