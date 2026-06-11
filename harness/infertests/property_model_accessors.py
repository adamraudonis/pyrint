class cache_readonly:
    def __init__(self, fget):
        self.fget = fget


class IndexOpsMixin:
    @cache_readonly
    def hasnans(self) -> bool:
        """Return True if there are any NaNs."""
        return False



class Series(IndexOpsMixin):
    hasnans = property(
        IndexOpsMixin.hasnans.fget,
        doc=IndexOpsMixin.hasnans.__doc__,
    )
