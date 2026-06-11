from propcache.api import cached_property


class A:
    @cached_property
    def latest_version(self):
        return "x"

    def f(self):
        if (latest_version := self.latest_version) is None:
            raise ValueError("nope")
        if latest_version == "y":
            return None
        return latest_version
