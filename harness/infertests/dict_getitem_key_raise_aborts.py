def make(a):
    states = {
        "media_player.test3": [],
        mp: [a],
        "media_player.test2": [a, a],
    }
    mp = "media_player.test"
    return states


def f():
    states = make("z")
    entity_states = states["media_player.test2"]
    print(len(entity_states))
    return entity_states
