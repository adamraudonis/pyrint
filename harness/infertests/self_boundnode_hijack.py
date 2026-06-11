class Q:
    default = "AND"

    def deconstruct(self):
        path = "%s.%s" % (self.__class__.__module__, self.__class__.__name__)
        if path.startswith("probe"):
            path = path.replace("probe", "models")
        return path
