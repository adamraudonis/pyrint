class ErrorDict(dict):
    pass


class ValidationError(Exception):
    def update_error_dict(self, error_dict):
        if hasattr(self, "error_dict"):
            return error_dict
        return ErrorDict()


class BaseForm:
    def __init__(self):
        self._errors = None

    def full_clean(self):
        self._errors = ErrorDict()


class Form(BaseForm):
    pass


def test():
    class CodeForm(Form):
        def clean(self):
            try:
                raise ValidationError()
            except ValidationError as e:
                self._errors = e.update_error_dict(self._errors)
