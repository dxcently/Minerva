import os
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import httpx
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import JSONResponse
from open_webui.utils.auth import get_admin_user

router = APIRouter(prefix="/api/v1/minerva/credentials", dependencies=[Depends(get_admin_user)])
ORIGIN = os.environ.get("MINERVA_EIDOLON_ORIGIN", "http://127.0.0.1:8085").rstrip("/")
if urlsplit(ORIGIN).hostname not in {"127.0.0.1", "localhost", "::1"}:
    raise RuntimeError("Credential backend must be local")


async def form_token(client, kind="operator"):
    token_path = os.environ.get("MINERVA_EIDOLON_TOKEN_FILE")
    try:
        bearer = Path(token_path).read_text(encoding="utf-8").strip() if token_path else ""
    except OSError:
        bearer = ""
    if not bearer:
        raise HTTPException(503, "Launch WebUI through hoot to enable operator controls")
    result = await client.get(ORIGIN + "/v1/operator/tokens", headers={"Authorization": "Bearer " + bearer})
    result.raise_for_status()
    token = result.json().get(kind)
    if not token:
        raise HTTPException(503, "Operator controls are unavailable")
    return token


def reply(data):
    return JSONResponse(data, headers={"Cache-Control": "no-store"})


@router.get("")
async def status():
    try:
        async with httpx.AsyncClient(timeout=10, trust_env=False) as client:
            result = await client.get(ORIGIN + "/api/auth")
            result.raise_for_status()
            return reply(result.json())
    except (httpx.HTTPError, ValueError):
        raise HTTPException(502, "Provider credentials are unavailable") from None


@router.post("/{action}")
async def update(action: str, request: Request):
    routes = {"set": "/api/secret/set", "delete": "/api/secret/delete", "recheck": "/api/auth/recheck"}
    if action not in routes:
        raise HTTPException(404)
    base = urlsplit(str(request.base_url))
    if request.headers.get("origin") != f"{base.scheme}://{base.netloc}":
        raise HTTPException(403, "Use WebUI settings to change credentials")
    if request.headers.get("sec-fetch-site") not in {None, "same-origin"}:
        raise HTTPException(403)
    if request.headers.get("content-type", "").split(";")[0] != "application/x-www-form-urlencoded":
        raise HTTPException(415)
    body = bytearray()
    async for chunk in request.stream():
        body.extend(chunk)
        if len(body) > 65536:
            raise HTTPException(413)
    try:
        fields = parse_qs(body.decode("utf-8"), keep_blank_values=True, max_num_fields=3)
    except (ValueError, UnicodeError):
        raise HTTPException(400, "Invalid credential form") from None
    finally:
        body[:] = b"\0" * len(body)
    data = {"name": fields.get("name", [""])[0]}
    if action == "set":
        data["value"] = fields.get("value", [""])[0]
    fields.clear()
    try:
        async with httpx.AsyncClient(timeout=10, trust_env=False) as client:
            data["form_token"] = await form_token(client)
            result = await client.post(ORIGIN + routes[action], data=data,
                headers={"Origin": ORIGIN, "Accept": "application/json"})
            if 400 <= result.status_code < 500:
                raise HTTPException(400, "Credential change refused; check the name, value and credential type")
            result.raise_for_status()
            outcome = result.json()
            if outcome.get("error"):
                raise HTTPException(400, "Credential change refused; check the name, value and credential type")
            return reply({"ok": True})
    except (httpx.HTTPError, ValueError):
        raise HTTPException(502, "Credential change failed") from None
    finally:
        data.clear()
