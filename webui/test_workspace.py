import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from fastapi import FastAPI
from fastapi.testclient import TestClient
from test_credentials import credentials, auth, HEADERS

spec = importlib.util.spec_from_file_location("workspace", Path(__file__).with_name("minerva_workspace.py"))
workspace = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, {"open_webui.utils.auth": auth, "open_webui.routers.minerva_credentials": credentials}):
    spec.loader.exec_module(workspace)
app = FastAPI(); app.include_router(workspace.router)
client = TestClient(app)
BASE = "/api/v1/minerva/workspace"


class WorkspaceTest(unittest.TestCase):
    def test_extension_controls_are_admin_only_and_allowlisted(self):
        self.assertEqual(client.get(BASE + "/extensions").status_code, 401)
        self.assertEqual(client.post(BASE + "/extensions/start", json={"name":"jev"}).status_code, 401)
        self.assertEqual(client.post(BASE + "/extensions/delete", headers=HEADERS, json={"name":"jev"}).status_code, 404)
        self.assertEqual(client.post(BASE + "/extensions/start", headers={**HEADERS,"Origin":"https://other.example"}, json={"name":"jev"}).status_code, 403)
        self.assertEqual(client.post(BASE + "/extensions/start", headers=HEADERS, json={"name":12}).status_code, 400)

    def test_inventory_requires_admin_and_forwards_no_chat_identity(self):
        self.assertEqual(client.get(BASE + "/capabilities").status_code, 401)
        seen = []
        async def upstream(path, **kwargs):
            seen.append((path, kwargs))
            return {"tools": [{"name":"bash"}], "skills":[]}
        with patch.object(workspace, "agent_headers", return_value={"Authorization":"Bearer test", "X-Eidolon-Chat-Id":"inventory"}), patch.object(workspace,"upstream",side_effect=upstream):
            result = client.get(BASE + "/capabilities", headers=HEADERS)
        self.assertEqual(result.status_code, 200)
        self.assertEqual(seen[0][0], "/v1/operator/capabilities")
        self.assertNotIn("X-Eidolon-Chat-Id", seen[0][1]["headers"])

    def test_files_require_admin(self):
        self.assertEqual(client.get(BASE + "/files").status_code, 401)

    def test_files_are_confined_to_enabled_workspace(self):
        with tempfile.TemporaryDirectory() as root, patch.object(workspace, "ROOT", Path(root)):
            for path in ["../outside", ".env", ".git/config"]:
                self.assertEqual(client.get(BASE + "/files", params={"path": path}, headers=HEADERS).status_code, 403)
            Path(root, "readme.md").write_text("# safe <script>text</script>", encoding="utf-8")
            result = client.get(BASE + "/files", params={"path": "readme.md"}, headers=HEADERS)
            self.assertEqual(result.json()["text"], "# safe <script>text</script>")
            self.assertTrue(result.json()["markdown"])

    def test_binary_and_large_files_refused(self):
        with tempfile.TemporaryDirectory() as root, patch.object(workspace, "ROOT", Path(root)):
            Path(root, "binary").write_bytes(b"\0\1")
            Path(root, "large").write_bytes(b"x" * 1048577)
            for path, status in [("binary", 415), ("large", 413)]:
                self.assertEqual(client.get(BASE + "/files", params={"path": path}, headers=HEADERS).status_code, status)

    def test_tool_allowlist_and_origin(self):
        self.assertEqual(client.post(BASE + "/tool", headers=HEADERS, json={"tool":"bash"}).status_code, 400)
        self.assertEqual(client.post(BASE + "/tool", headers={**HEADERS,"Origin":"https://other.example"}, json={"tool":"jev_run"}).status_code, 403)

    def test_save_refuses_stale_editor(self):
        async def upstream(*args, **kwargs):
            return {"graphs":[{"id":"test","json":"newer"}]}
        with patch.object(workspace,"upstream",side_effect=upstream):
            result = client.post(BASE + "/graphs/save",headers=HEADERS,json={"id":"test","graph":"edit","expected":"old"})
        self.assertEqual(result.status_code,409)


if __name__ == "__main__":
    unittest.main()
