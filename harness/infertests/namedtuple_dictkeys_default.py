from collections import namedtuple


def make(state, events):
    d = {"current_state": state, "events": events}
    iv = namedtuple("IV", d.keys())(*d.values())
    return iv
