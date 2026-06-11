import multiprocessing

class C:
    def setup(self):
        self.manager = multiprocessing.Manager()

    def __exit__(self, *args):
        self.manager.__exit__(*args)
