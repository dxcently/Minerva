import os
from pathlib import Path

import httpx
from markdown_it import MarkdownIt
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import JSONResponse
from open_webui.utils.auth import get_admin_user
from open_webui.routers.minerva_credentials import ORIGIN, form_token

router = APIRouter(prefix="/api/v1/minerva/workspace", dependencies=[Depends(get_admin_user)])
ROOT = Path(os.environ.get("MINERVA_WORKSPACE", os.getcwd())).resolve()
HIDDEN = {".git", ".env", "node_modules", "target", "models", ".venv", "__pycache__"}


def response(data):
    return JSONResponse(data, headers={"Cache-Control": "no-store"})


def workspace_path(name):
    path = (ROOT / name).resolve()
    if not path.is_relative_to(ROOT) or any(part in HIDDEN or part.startswith(".env") for part in path.relative_to(ROOT).parts):
        raise HTTPException(403, "File is outside the enabled workspace")
    return path


async def body(request):
    from urllib.parse import urlsplit
    base = urlsplit(str(request.base_url))
    if request.headers.get("origin") != f"{base.scheme}://{base.netloc}":
        raise HTTPException(403)
    content = bytearray()
    async for chunk in request.stream():
        content.extend(chunk)
        if len(content) > 1048576:
            raise HTTPException(413)
    import json
    try:
        result = json.loads(content)
        if not isinstance(result, dict):
            raise ValueError()
        return result
    except (ValueError, UnicodeError):
        raise HTTPException(400, "Expected a JSON object") from None


async def upstream(path, *, method="GET", data=None, headers=None):
    try:
        async with httpx.AsyncClient(timeout=15, trust_env=False) as client:
            result = await client.request(method, ORIGIN + path, data=data, headers=headers)
            if result.status_code == 404:
                raise HTTPException(503, "This feature needs the updated Eidolon server")
            if result.status_code >= 400:
                raise HTTPException(result.status_code, "Eidolon refused the request")
            return result.json()
    except (httpx.HTTPError, ValueError):
        raise HTTPException(502, "Eidolon is unavailable") from None


@router.get("/files")
async def files(path: str = ""):
    target = workspace_path(path)
    if not target.exists():
        raise HTTPException(404)
    if target.is_dir():
        entries = []
        for item in sorted(target.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower())):
            if item.name in HIDDEN or item.name.startswith(".env") or item.is_symlink():
                continue
            entries.append({"name": item.name, "path": item.relative_to(ROOT).as_posix(), "directory": item.is_dir()})
            if len(entries) == 500:
                break
        return response({"path": path, "entries": entries})
    if target.stat().st_size > 1048576:
        raise HTTPException(413, "Preview is limited to 1 MiB")
    try:
        text = target.read_text(encoding="utf-8")
        if "\0" in text:
            raise ValueError()
    except (UnicodeError, ValueError):
        raise HTTPException(415, "This file is not text") from None
    markdown = target.suffix.lower() in {".md", ".markdown"}
    rendered = MarkdownIt("commonmark", {"html": False}).disable("image").render(text) if markdown else None
    return response({"path": path, "text": text, "markdown": markdown, "html": rendered})


@router.get("/graphs")
async def graphs():
    return response(await upstream("/api/jev"))


@router.get("/runs")
async def runs():
    return response(await upstream("/api/jev/runs"))


@router.post("/graphs/{action}")
async def graph_action(action: str, request: Request):
    if action not in {"save", "lint", "delete"}:
        raise HTTPException(404)
    payload = await body(request)
    data = {"id": payload.get("id", ""), "graph": payload.get("graph", "")}
    if not isinstance(data["graph"], str):
        raise HTTPException(400)
    if action != "lint":
        store = await upstream("/api/jev")
        current = next((g for g in store.get("graphs", []) if g["id"] == data["id"]), None)
        if current and current["json"] != payload.get("expected"):
            raise HTTPException(409, "Graph changed on disk. Reload before saving.")
        if not current and payload.get("expected"):
            raise HTTPException(409, "Graph was deleted. Reload before saving.")
    try:
        async with httpx.AsyncClient(timeout=10, trust_env=False) as client:
            data["form_token"] = await form_token(client, "graph")
    except httpx.HTTPError:
        raise HTTPException(502, "Eidolon is unavailable") from None
    if action == "delete":
        data["delete_confirmed"] = "yes"
    return response(await upstream("/api/jev/" + action, method="POST", data=data,
        headers={"Origin": ORIGIN, "Accept": "application/json"}))


def agent_headers(chat):
    if not chat or len(chat) > 256:
        raise HTTPException(400, "Open a saved chat first")
    token_path = os.environ.get("MINERVA_EIDOLON_TOKEN_FILE")
    if not token_path:
        raise HTTPException(503, "Launch WebUI through hoot to enable agent controls")
    try:
        token = Path(token_path).read_text(encoding="utf-8").strip()
    except OSError:
        raise HTTPException(503, "Eidolon authentication is unavailable") from None
    return {"Authorization": "Bearer " + token, "X-Eidolon-Chat-Id": chat, "Content-Type": "application/json"}


@router.get("/activity")
async def activity(chat: str):
    return response(await upstream("/v1/agent/activity", headers=agent_headers(chat)))


@router.post("/tool")
async def tool(request: Request):
    import json
    payload = await body(request)
    if payload.get("tool") not in {"jev_run", "jev_resume", "jev_stop", "jev_runs", "jev_order"}:
        raise HTTPException(400, "Unsupported run control")
    headers = agent_headers(payload.get("chat"))
    return response(await upstream("/v1/agent/tool", method="POST", headers=headers,
        data=json.dumps({"model": payload.get("model", "eidolon"), "tool": payload["tool"], "input": payload.get("input", {})})))


@router.post("/answer")
async def answer(request: Request):
    import json
    payload = await body(request)
    if not isinstance(payload.get("question"), str) or not isinstance(payload.get("approved"), bool):
        raise HTTPException(400, "Expected a displayed question and approval")
    return response(await upstream("/v1/agent/answer", method="POST", headers=agent_headers(payload.get("chat")),
        data=json.dumps({"question": payload["question"], "approved": payload["approved"]})))


@router.get("/extensions")
async def extensions():
    return response(await upstream("/api/ext"))


@router.post("/extensions/{action}")
async def extension_action(action: str, request: Request):
    if action not in {"enable", "disable", "start", "stop", "restart"}:
        raise HTTPException(404)
    payload = await body(request)
    name = payload.get("name")
    if not isinstance(name, str) or not name.strip():
        raise HTTPException(400, "Select an extension")
    try:
        async with httpx.AsyncClient(timeout=10, trust_env=False) as client:
            token = await form_token(client)
    except (httpx.HTTPError, ValueError):
        raise HTTPException(502, "Extension controls are unavailable") from None
    result = await upstream("/api/ext/" + action, method="POST", data={"name": name, "form_token": token},
        headers={"Origin": ORIGIN, "Accept": "application/json"})
    return response({key: value for key, value in result.items() if key != "html"})


@router.get("/capabilities")
async def capabilities():
    headers = agent_headers("inventory")
    headers.pop("X-Eidolon-Chat-Id", None)
    return response(await upstream("/v1/operator/capabilities", headers=headers))
