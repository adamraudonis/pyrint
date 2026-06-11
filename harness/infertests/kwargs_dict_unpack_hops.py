class T:
    def build_expected_result(self, **kwargs):
        return {"project.name": None, **kwargs}

    def test(self):
        data = self.build_expected_result(
            id="x",
            check_status="success",
            http_status_code=200,
            region="us-east-1",
        )
        print(data)
