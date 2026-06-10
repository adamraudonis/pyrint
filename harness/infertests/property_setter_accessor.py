class PropertyTest:
    @property
    def test(self):
        return 42

    @test.setter
    def test(self, value):
        pass

p = PropertyTest()
p.test
PropertyTest.test
