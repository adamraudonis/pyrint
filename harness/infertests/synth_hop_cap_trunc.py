PW = "pw"


def t_implicit():
    try:
        try:
            raise RuntimeError(f"Inner with {PW}")
        except RuntimeError:
            raise RuntimeError(f"Outer with {PW}")
    except RuntimeError as exc:
        captured_exc = exc

    print(str(captured_exc.args[0]))
    print(str(captured_exc.__context__.args[0]))


def t_pairs_0():
    exc1 = RuntimeError(f"E1_0 {PW}")
    exc2 = RuntimeError(f"E2_0 {PW}")
    exc1.__context__ = exc2
    exc2.__context__ = exc1
    print(str(exc1.args[0]))
    print(str(exc2.args[0]))
    print(str(exc1.__context__.args[0]))


def t_pairs_1():
    exc1 = RuntimeError(f"E1_1 {PW}")
    exc2 = RuntimeError(f"E2_1 {PW}")
    exc1.__context__ = exc2
    exc2.__context__ = exc1
    print(str(exc1.args[0]))
    print(str(exc2.args[0]))
    print(str(exc1.__context__.args[0]))


def t_pairs_2():
    exc1 = RuntimeError(f"E1_2 {PW}")
    exc2 = RuntimeError(f"E2_2 {PW}")
    exc1.__context__ = exc2
    exc2.__context__ = exc1
    print(str(exc1.args[0]))
    print(str(exc2.args[0]))
    print(str(exc1.__context__.args[0]))


def t_pairs_3():
    exc1 = RuntimeError(f"E1_3 {PW}")
    exc2 = RuntimeError(f"E2_3 {PW}")
    exc1.__context__ = exc2
    exc2.__context__ = exc1
    print(str(exc1.args[0]))
    print(str(exc2.args[0]))
    print(str(exc1.__context__.args[0]))

