from unresolvable_xyz import HTTPAdapter

class JWTRefreshAdapter(HTTPAdapter):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def send(self, request, **kwargs):
        response = super().send(request, **kwargs)
        return response
