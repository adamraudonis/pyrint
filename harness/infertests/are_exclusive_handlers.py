def f(go):
    x = None
    try:
        x = go()
    except OSError:
        x = None
    except Exception:
        if x:
            print(x)
