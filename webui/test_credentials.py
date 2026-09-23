import importlib.util
import sys
import types
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs

import httpx
from fastapi import FastAPI, Header, HTTPException
from fastapi.testclient import TestClient


def admin(authorization: str = Header(default="")):
    if authorization != "Bearer test-admin":
        raise HTTPException(401)


auth = types.ModuleType("open_webui.utils.auth")
auth.get_admin_user = admin
spec = importlib.util.spec_from_file_location("credentials", Path(__file__).with_name("minerva_credentials.py"))
credentials = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, {"open_webui.utils.auth": auth}):
    spec.loader.exec_module(credentials)
app = FastAPI()
app.include_router(credentials.router)
client = TestClient(app)
HEADERS = {"Authorization": "Bearer test-admin", "Origin": "http://testserver"}
BASE = "/api/v1/minerva/credentials"


class CredentialsTest(unittest.TestCase):
    def test_requires_admin_dependency(self):
        self.assertEqual(client.get(BASE).status_code, 401)
        self.assertEqual(client.post(BASE + "/set", data={"name": "test"}).status_code, 401)

    def test_foreign_origin_refused_before_backend(self):
        with patch.object(credentials.httpx, "AsyncClient") as backend:
            result = client.post(BASE + "/set", headers={**HEADERS, "Origin": "https://other.example"}, data={})
            self.assertEqual(result.status_code, 403)
            backend.assert_not_called()

    def test_unknown_action_refused(self):
        self.assertEqual(client.post(BASE + "/arbitrary", headers=HEADERS).status_code, 404)

    def test_save_uses_token_and_does_not_echo_value(self):
        seen = []
        def backend(request):
            seen.append(request)
            if request.url.path == "/v1/operator/tokens":
                return httpx.Response(200, json={"operator":"test-token"})
            return httpx.Response(200, json={"key": {"name": "test"}, "html": "unused"})
        real_client = httpx.AsyncClient
        with patch.object(credentials.Path, "read_text", return_value="test-bearer"), patch.dict(credentials.os.environ, {"MINERVA_EIDOLON_TOKEN_FILE":"test-token"}), patch.object(credentials.httpx, "AsyncClient", side_effect=lambda **kwargs: real_client(
                transport=httpx.MockTransport(backend), **kwargs)):
            result = client.post(BASE + "/set", headers=HEADERS,
                data={"name": "test", "value": "sentinel-secret", "ignored": "drop"})
        self.assertEqual(result.status_code, 200)
        self.assertNotIn("sentinel-secret", result.text)
        self.assertEqual(result.headers["cache-control"], "no-store")
        self.assertEqual(seen[0].headers["authorization"], "Bearer test-bearer")
        fields = parse_qs(seen[-1].content.decode())
        self.assertEqual(fields, {"name": ["test"], "value": ["sentinel-secret"], "form_token": ["test-token"]})
        self.assertEqual(seen[-1].url.path, "/api/secret/set")

    def test_body_limit(self):
        result = client.post(BASE + "/set", headers=HEADERS, data={"value": "x" * 65537})
        self.assertEqual(result.status_code, 413)


if __name__ == "__main__":
    unittest.main()
