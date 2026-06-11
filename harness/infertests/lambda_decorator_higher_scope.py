import pytest


@pytest.mark.filterwarnings("ignore::DeprecationWarning")
@pytest.mark.parametrize("copy", [True, None, False])
@pytest.mark.parametrize(
    "method",
    [
        lambda df, copy: df.rename(columns=str.lower, copy=copy),
    ],
)
def test_x(request, method, copy):
    y = copy
