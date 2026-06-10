class T:
    def settings(self, **kw):
        return None

    def make_request(self, body):
        return body

    def test(self):
        body = b"x" * 5
        with self.settings(DATA_UPLOAD_MAX_MEMORY_SIZE=5):
            self.assertEqual(self.make_request(body).body, body)
