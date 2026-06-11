class H:
    def __init__(self, a, b, c):
        self._a = a
        self._b = b
        self._c = str(c)

    def build(self, cmd):
        connection_cmd = ["spark-sql"]
        if self._a:
            connection_cmd += ["--a", str(self._a)]
        if self._b:
            connection_cmd += ["--b", self._b]
        if self._c:
            connection_cmd += ["--c", self._c]

        if isinstance(cmd, str):
            connection_cmd += cmd.split()
        elif isinstance(cmd, list):
            connection_cmd += cmd

        print("cmd: %s", connection_cmd)

        return connection_cmd
