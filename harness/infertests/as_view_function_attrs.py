class View:
    """
    Intentionally simple parent class for all views.
    """

    @classmethod
    def as_view(cls, **initkwargs):
        def view(request, *args, **kwargs):
            return request

        view.view_class = cls
        view.__doc__ = cls.__doc__
        view.__module__ = cls.__module__
        view.__annotations__ = cls.dispatch.__annotations__
        return view

    def dispatch(self, request):
        return request


class SimpleView(View):
    """
    A simple view with a docstring.
    """


def test():
    cls = SimpleView
    view = cls.as_view()
    print(view.__doc__, cls.__doc__)
    print(view.__name__, view.__module__, cls.__module__)
    print(view.__qualname__, view.__annotations__)
