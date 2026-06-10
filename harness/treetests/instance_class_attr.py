class CallableWithoutName:
    def __call__(self):
        return 1


callback = CallableWithoutName()
callback.__class__.__name__ = "Renamed"
