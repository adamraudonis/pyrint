def f(x):
    match x:
        case {"a": 1, "b": str() as s, **rest}:
            return s, rest
        case {"event_id": str() as event_id}:
            return event_id
