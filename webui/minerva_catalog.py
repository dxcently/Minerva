import asyncio
import time
from functools import wraps

from fastapi import HTTPException
from open_webui.routers.minerva_workspace import agent_headers, upstream


class Catalog:
    def __init__(self):
        self.lock = asyncio.Lock()
        self.expires = 0
        self.data = {}

    async def read(self):
        async with self.lock:
            if time.monotonic() >= self.expires:
                headers = agent_headers("inventory")
                headers.pop("X-Eidolon-Chat-Id", None)
                self.data = await upstream("/v1/operator/capabilities", headers=headers)
                self.expires = time.monotonic() + 5
            return self.data


def runtime_rows(category, data):
    if category not in {"tools", "skills"}:
        raise ValueError("Runtime Workspace entries must be tools or skills")
    entries = data.get(category, [])
    if category == "tools":
        entries = list({row["name"]: row for row in entries + data.get("loaded_tools", [])}.values())
    rows = []
    for entry in entries:
        name = entry.get("name") or entry["id"]
        row = dict(id="runtime:" + name,
                   name=name, user_id="runtime", write_access=False, access_grants=[],
                   user={"id": "runtime", "name": "Minerva", "role": "system", "email": ""},
                   created_at=0, updated_at=0, meta={}, is_active=True)
        description = entry.get("description", "")
        if category == "tools":
            row.update(meta={"description": description, "manifest": entry}, specs=[{
                "name": name, "description": description,
                "parameters": entry.get("input_schema", {})}])
        else:
            row.update(description=description, content=entry.get("prompt") or description)
        rows.append(row)
    return rows


def as_dict(value):
    return value.model_dump() if hasattr(value, "model_dump") else value


def merge_rows(saved, runtime, options):
    query = (options.get("query") or "").casefold()
    view = options.get("view_option")
    if view == "created" or options.get("tag"):
        runtime = []
    runtime = [row for row in runtime if query in (row["name"] + " " + row["id"]).casefold()]
    rows = {row["id"]: row for row in runtime}
    rows.update((row["id"], row) for row in saved)
    key = options.get("order_by") or "updated_at"
    if key not in {"name", "created_at", "updated_at"}:
        key = "updated_at"
    return sorted(rows.values(), key=lambda row: ((row.get(key) or "").casefold() if key == "name" else row.get(key, 0), row["id"]),
                  reverse=options.get("direction") != "asc")


def list_handler(original, category, catalog):
    @wraps(original)
    async def listed(**kwargs):
        user = kwargs["user"]
        if user.role != "admin":
            return await original(**kwargs)
        data = await catalog.read()
        first = as_dict(await original(**{**kwargs, "page": 1})) if "page" in kwargs else await original(**kwargs)
        if isinstance(first, list):
            return merge_rows([as_dict(row) for row in first], runtime_rows(category, data), kwargs)
        saved = [as_dict(row) for row in first["items"]]
        page = 2
        while len(saved) < first["total"]:
            batch = as_dict(await original(**{**kwargs, "page": page}))
            if not batch["items"]:
                raise HTTPException(409, "Workspace changed while listing; refresh")
            saved.extend(as_dict(row) for row in batch["items"])
            page += 1
        rows = merge_rows(saved, runtime_rows(category, data), kwargs)
        size = 30
        start = (max(1, kwargs.get("page") or 1) - 1) * size
        return {"items": rows[start:start + size], "total": len(rows)}
    return listed


def detail_handler(original, category, catalog):
    @wraps(original)
    async def detail(**kwargs):
        try:
            saved = await original(**kwargs)
            if saved is not None:
                return saved
        except HTTPException as error:
            if error.status_code != 404:
                raise
        if kwargs["user"].role == "admin":
            for row in runtime_rows(category, await catalog.read()):
                if row["id"] == kwargs["id"]:
                    return row
        raise HTTPException(404, "Workspace item not found")
    return detail


def install(app):
    catalog = Catalog()
    paths = {
        "/api/v1/tools/list": ("tools", list_handler),
        "/api/v1/skills/list": ("skills", list_handler),
        "/api/v1/skills/id/{id}": ("skills", detail_handler),
    }
    found = set()
    for route in app.routes:
        if getattr(route, "path", None) in paths and "GET" in route.methods:
            category, wrapper = paths[route.path]
            if not getattr(route.dependant.call, "minerva_catalog", False):
                route.dependant.call = wrapper(route.dependant.call, category, catalog)
                route.dependant.call.minerva_catalog = True
            found.add(route.path)
    if found != paths.keys():
        raise RuntimeError("WebUI Workspace routes changed: " + ", ".join(paths.keys() - found))
