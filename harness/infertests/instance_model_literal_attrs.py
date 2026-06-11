class Shared:
    backend_class = None

    def m(self):
        print(self.backend_class.__module__)
        x = [1, 2]
        print(x.__module__)
        print((5).__module__)
        print("s".__module__)
        print({}.__module__)
        print({}.__doc__)
        print({}.__dict__)
        print({}.__class__)
        print([].__doc__)
        print([].__dict__)
        print((1, 2).__module__)
