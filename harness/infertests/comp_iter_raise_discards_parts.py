def dh(c):
    if c:
        return {"show": True, "back": {"x": 1}, "choices": [{"title": 2}], "f": 3}
    elif c == 2:
        return {"show": True, "back": 1, "choices": [{"link": 4} for d in c], "f": 5}
    else:
        return {"show": True, "back": None, "choices": [{"link": 6} for y in c], "f": 7}

spec = dh(9)
choices = [choice["link"] for choice in spec["choices"]]
choices
