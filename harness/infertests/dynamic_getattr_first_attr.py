from missing_module import PluggableViewMixin
from threading import local


class IPlugin(local, PluggableViewMixin):
    def get_title(self):
        return self.title
