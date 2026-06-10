class ND:
    def copy(self):
        return ND()

    def _update(self, r):
        return None

    def sort(self, by, inplace=False):
        result = self.copy()
        if inplace:
            return self._update(result)
        return result


x = ND().sort("a")
x
d = {"a": 1}
y = d.copy()
y
