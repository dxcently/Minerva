import importlib.util
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fastapi import Depends, FastAPI, HTTPException
from fastapi.testclient import TestClient
from test_workspace import workspace

spec = importlib.util.spec_from_file_location("catalog", Path(__file__).with_name("minerva_catalog.py"))
catalog = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, {"open_webui.routers.minerva_workspace": workspace}):
    spec.loader.exec_module(catalog)

ADMIN = SimpleNamespace(id="owner", role="admin")
USER = SimpleNamespace(id="member", role="user")
DATA = {"tools": [{"name": "bash", "description": "Run a command"}],
        "loaded_tools": [{"name": "bash", "description": "Run a command"}],
        "models": [{"id": "local:model", "name": "Local model"}],
        "skills": [{"name": "triage", "description": "Investigate"}]}


class CatalogTest(unittest.IsolatedAsyncioTestCase):
    async def test_tools_share_one_list_without_loaded_duplicates(self):
        original = AsyncMock(return_value=[{"id": "saved", "name": "Saved tool", "updated_at": 4}])
        read = SimpleNamespace(read=AsyncMock(return_value=DATA))
        rows = await catalog.list_handler(original, "tools", read)(user=ADMIN)
        self.assertEqual({row["id"] for row in rows}, {"saved", "runtime:bash"})
        self.assertFalse(next(row for row in rows if row["id"] == "runtime:bash")["write_access"])

    async def test_saved_entries_keep_their_edit_permissions(self):
        saved = [{"id": "saved", "name": "My tool", "write_access": True}]
        rows = catalog.merge_rows(saved, [], {})
        self.assertEqual(rows, saved)

    async def test_runtime_inventory_not_exposed_to_other_users(self):
        original = AsyncMock(return_value=[])
        read = SimpleNamespace(read=AsyncMock())
        self.assertEqual(await catalog.list_handler(original, "tools", read)(user=USER), [])
        read.read.assert_not_called()

    async def test_merge_before_pagination_and_shared_search(self):
        saved = [{"id": str(i), "name": f"A{i:02}", "updated_at": i} for i in range(35)]
        async def original(page, **kwargs):
            return {"items": saved[(page-1)*30:page*30], "total": 35}
        read = SimpleNamespace(read=AsyncMock(return_value=DATA))
        wrapper = catalog.list_handler(original, "skills", read)
        options = dict(user=ADMIN, order_by="name", direction="asc")
        first = await wrapper(page=1, **options)
        second = await wrapper(page=2, **options)
        self.assertEqual((len(first["items"]), len(second["items"]), first["total"]), (30, 6, 36))
        self.assertEqual(second["items"][-1]["id"], "runtime:triage")
        rows = catalog.merge_rows([], catalog.runtime_rows("tools", DATA), {"query": "BASH"})
        self.assertEqual(len(rows), 1)
        self.assertEqual(catalog.merge_rows([], rows, {"view_option": "created"}), [])

    async def test_runtime_failure_is_not_reported_as_empty(self):
        read = SimpleNamespace(read=AsyncMock(side_effect=HTTPException(502, "Unavailable")))
        with self.assertRaises(HTTPException):
            await catalog.list_handler(AsyncMock(return_value=[]), "tools", read)(user=ADMIN)

    async def test_cache_no_chat_identity_and_retry_after_failure(self):
        instance = catalog.Catalog()
        with patch.object(catalog, "agent_headers", return_value={"Authorization": "Bearer test", "X-Eidolon-Chat-Id": "inventory"}), patch.object(catalog, "upstream", new=AsyncMock(return_value=DATA)) as upstream:
            await instance.read()
            await instance.read()
            self.assertEqual(upstream.await_count, 1)
            self.assertNotIn("X-Eidolon-Chat-Id", upstream.call_args.kwargs["headers"])

    async def test_detail_does_not_bypass_native_denial(self):
        read = SimpleNamespace(read=AsyncMock(return_value=DATA))
        original = AsyncMock(side_effect=HTTPException(403))
        with self.assertRaises(HTTPException) as error:
            await catalog.detail_handler(original, "skills", read)(id="runtime:triage", user=ADMIN)
        self.assertEqual(error.exception.status_code, 403)
        read.read.assert_not_called()

    async def test_install_leaves_presets_model_picker_and_native_editors_untouched(self):
        app = FastAPI()
        async def native():
            return {"native": True}
        for path in ["/api/v1/tools/list", "/api/v1/skills/list", "/api/v1/skills/id/{id}"]:
            app.add_api_route(path, native, methods=["GET"])
        routes = [
            ("/api/v1/models/list", "GET"), ("/api/v1/models/model", "GET"),
            ("/api/models", "GET"),
            ("/api/v1/models/create", "POST"), ("/api/v1/models/model/update", "POST"),
            ("/api/v1/tools/create", "POST"), ("/api/v1/tools/id/{id}/update", "POST"),
            ("/api/v1/skills/create", "POST"), ("/api/v1/skills/id/{id}/update", "POST"),
        ]
        for path, method in routes:
            app.add_api_route(path, native, methods=[method])
        catalog.install(app)
        for route in app.routes:
            if any(route.path == path and method in route.methods for path, method in routes):
                self.assertIs(route.dependant.call, native, route.path)
        with self.assertRaises(ValueError):
            catalog.runtime_rows("models", DATA)

    async def test_wrapping_keeps_fastapi_authentication(self):
        app = FastAPI()
        def denied():
            raise HTTPException(401)
        @app.get("/api/v1/tools/list")
        async def listing(user=Depends(denied)):
            return []
        route = app.routes[-1]
        route.dependant.call = catalog.list_handler(route.dependant.call, "tools", SimpleNamespace(read=AsyncMock(return_value=DATA)))
        with TestClient(app) as client:
            self.assertEqual(client.get("/api/v1/tools/list").status_code, 401)
            app.dependency_overrides[denied] = lambda: ADMIN
            response = client.get("/api/v1/tools/list")
            self.assertEqual(response.status_code, 200)
            self.assertEqual(len(response.json()), 1)


if __name__ == "__main__":
    unittest.main()
