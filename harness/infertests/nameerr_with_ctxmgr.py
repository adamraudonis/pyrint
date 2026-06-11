from missing_mod import Hook

def run():
    with (
        Hook(1) as hook,
        hook.invoke() as ps,
    ):
        ps.add_command(1)
        x = ps.had_errors
        use(x)

def run2():
    with Hook(2) as h2:
        h2.foo()

def run3():
    with undefined_name() as h3:
        h3.foo()
