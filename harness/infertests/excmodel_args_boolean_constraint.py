class MyErr(Exception):
    def __str__(self):
        return str(self.args[0]) if self.args else ""

class C:
    def __init__(self):
        self.vals = ()
    def go(self):
        return str(self.vals[0]) if self.vals else ""
