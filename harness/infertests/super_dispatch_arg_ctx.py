class B:
    def dispatch(self, request, *args, **kwargs):
        return request

class V(B):
    def dispatch(self, request, *args, **kwargs):
        return super().dispatch(request, *args, **kwargs)
