def outer(config):
    event_data_items = None
    if "x" in config:
        event_data = {}
        event_data.update(config["x"])
        if any(isinstance(v, list) for v in event_data.values()):
            pass
        else:
            event_data_items = event_data.items()

    def filter_event(ed):
        if event_data_items:
            if not (ed.items() >= event_data_items):
                return False
        return True

    return filter_event
