class Test:
    lam = lambda self, icon: (self, icon)

    def test(self):
        return self.lam(42)

x = Test().lam(1)
x
y = Test().test()
y
