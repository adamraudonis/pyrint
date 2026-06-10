class QuerySet:
    def as_manager(cls):
        return 1

    as_manager.queryset_only = False
    as_manager = classmethod(as_manager)


QuerySet.as_manager
QuerySet.as_manager()
