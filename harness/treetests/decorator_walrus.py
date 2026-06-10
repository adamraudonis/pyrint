import unittest.mock as mock


class TestHook:
    @mock.patch(f"{(mock_client := 'kubernetes.client')}.ApiClient")
    def test_apply(self, mock_api_client):
        return mock_api_client


def fn_ret_walrus(x) -> (rw := int):
    return x
