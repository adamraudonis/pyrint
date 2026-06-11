class ChoiceField:
    """Docstring is skipped by get_children."""

    @property
    def choices(self):
        return self._choices

    @choices.setter
    def choices(self, value):
        self._choices = value


class WithAttr:
    widget = None

    @property
    def choices(self):
        return self._choices

    @choices.setter
    def choices(self, value):
        self._choices = value


class ExtensionArray:
    @classmethod
    def _from_sequence_of_strings(cls, strings):
        return strings


x = ChoiceField.choices.fset
z = WithAttr.choices.fset
y = ExtensionArray._from_sequence_of_strings.__func__
