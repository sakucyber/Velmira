"""Pairing endpoints; pairing grants reading access, never local administration."""
import io

from fastapi import APIRouter, Body, Request
from fastapi.responses import JSONResponse, Response

from .. import security as s

router = APIRouter()


@router.get("/api/auth/status")
def auth_status(request: Request):
    value = s.public_settings(request)
    return {"enabled": value["enabled"], "authenticated": s.authenticated(request), "local": value["local"]}


@router.post("/api/auth/login")
def login(request: Request, payload: dict = Body(...)):
    if set(payload) != {"token"}:
        return s._error(400, "invalid_request", "请填写配对码")
    cookie = s.login(request, payload["token"])
    if not cookie:
        return s._error(401, "invalid_pairing_code", "配对码无效、已更换或尝试过于频繁")
    response = JSONResponse({"ok": True})
    response.set_cookie(s.COOKIE_NAME, cookie, max_age=s.SESSION_SECONDS, httponly=True,
                        secure=request.url.scheme == "https", samesite="strict", path="/")
    return response


@router.post("/api/auth/logout")
def logout(request: Request):
    s.logout(request)
    response = JSONResponse({"ok": True})
    response.delete_cookie(s.COOKIE_NAME, path="/", httponly=True, samesite="strict")
    return response


@router.get("/api/security/settings")
def get_settings(request: Request):
    return {"settings": s.public_settings(request)}


@router.patch("/api/security/settings")
def patch_settings(request: Request, payload: dict = Body(...)):
    if not s.is_local(request):
        return s._error(403, "local_admin_required", "仅本机可管理局域网访问")
    try:
        if set(payload) != {"enabled"}:
            raise ValueError("请只提交 enabled 设置")
        s.set_enabled(payload["enabled"])
        return {"settings": s.public_settings(request)}
    except ValueError as exc:
        return s._error(400, "invalid_security_settings", str(exc))


@router.post("/api/security/token")
def rotate_token(request: Request, payload: dict = Body(default={})):
    if not s.is_local(request):
        return s._error(403, "local_admin_required", "仅本机可生成配对码")
    if payload:
        return s._error(400, "invalid_request", "生成配对码无需其他参数")
    token = s.rotate_token()
    pairing_url, expires = s.create_pairing_link(request)
    return {"token": token, "pairing_url": pairing_url, "pairing_expires": expires, "settings": s.public_settings(request)}


@router.post("/api/security/qr")
def pairing_qr(request: Request, payload: dict = Body(...)):
    """Render a pairing deep link as a local-only QR image.

    QR generation is optional so the core reader still starts on minimal
    installations.  The desktop UI falls back to showing the deep link when
    the optional qrcode package is unavailable.
    """
    if not s.is_local(request):
        return s._error(403, "local_admin_required", "仅本机可生成连接二维码")
    if set(payload) != {"data"} or not isinstance(payload.get("data"), str) or not 1 <= len(payload["data"]) <= 2048:
        return s._error(400, "invalid_request", "二维码内容无效")
    try:
        import qrcode
    except ImportError:
        return s._error(503, "qr_unavailable", "当前安装未包含二维码组件，请复制连接链接")
    image = qrcode.make(payload["data"])
    output = io.BytesIO()
    image.save(output, format="PNG")
    return Response(output.getvalue(), media_type="image/png", headers={"Cache-Control": "no-store"})


@router.post("/api/security/invite")
def invite(request: Request, payload: dict = Body(default={})):
    """Add another reader without revoking previously paired devices."""
    if not s.is_local(request):
        return s._error(403, "local_admin_required", "仅本机可连接新设备")
    if payload:
        return s._error(400, "invalid_request", "连接新设备无需其他参数")
    value = s.load_settings()
    token = None
    if not value["token_hash"]:
        token = s.rotate_token()
    else:
        s.set_enabled(True)
    pairing_url, expires = s.create_pairing_link(request)
    return {"token": token, "pairing_url": pairing_url, "pairing_expires": expires,
            "settings": s.public_settings(request)}
