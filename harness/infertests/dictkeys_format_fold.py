RESOURCES_METHODS = {"a": 1, "b": 2}


def f(resource):
    available_resources = RESOURCES_METHODS.keys()
    if resource not in available_resources:
        error_message = f"Resource not found! Available Resources: {available_resources}"
        raise ValueError(error_message)
