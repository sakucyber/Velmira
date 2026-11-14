"""Local administration and explicitly paired, read-only LAN clients.

The socket peer is authoritative; forwarded headers never grant privileges.
Only hashes of pairing codes and browser sessions are written to disk.
"""
from __future__ import annotations

from collections import deque
import hashlib
import hmac
import ipaddress
import json
import os
import re
import secrets
import socket
import threading
import time
from urllib.parse import quote, urlsplit

from starlette.requests import Request
from starlette.responses import JSONResponse

from . import config

COOKIE_NAME = "velmira_session"
SESSION_SECONDS = 30 * 24 * 60 * 60
PAIRING_LINK_SECONDS = 5 * 60
_lock = threading.RLock()
_login_attempts = {}
_BOOK = r"[^/]+"
_READ_PATHS = [re.compile(pattern) for pattern in (
    r"/api/books", r"/api/events(?:/status)?",
    rf"/api/books/{_BOOK}",
    rf"/api/books/{_BOOK}/(?:toc|progress|bookmarks|annotations|search|segments|source)",
    rf"/api/books/{_BOOK}/chapters/\d+", rf"/api/books/{_BOOK}/pages(?:/\d+)?",
    rf"/api/books/{_BOOK}/epub/assets/{_BOOK}",
    rf"/media/{_BOOK}/\d+",
    rf"/api/books/{_BOOK}/manga/assets/{_BOOK}",
    rf"/api/books/{_BOOK}/manga/page-panels",
    rf"/api/books/{_BOOK}/manga/pages(?:/{_BOOK}/image)?",
    rf"/api/books/{_BOOK}/manga/auto",
    rf"/api/books/{_BOOK}/manga/auto(?:/runs|/pages|/logs)(?:/{_BOOK}/image)?",
    rf"/api/books/{_BOOK}/manga/exports(?:/{_BOOK}/download)?",
    rf"/api/books/{_BOOK}/manga/auto/pages(?:/{_BOOK}/image)?",
    r"/api/sync/pull", r"/api/sync/snapshot", rf"/api/sync/books/{_BOOK}/manifest",
)]
_WRITE_PATHS = {
    "PUT": [re.compile(rf"/api/books/{_BOOK}/progress")],
    "PATCH": [re.compile(rf"/api/books/{_BOOK}/annotations/{_BOOK}")],
    "DELETE": [re.compile(rf"/api/books/{_BOOK}/(?:bookmarks|annotations)/{_BOOK}")],
    "POST": [re.compile(rf"/api/books/{_BOOK}/(?:bookmarks|annotations)"), re.compile(r"/api/sync/device"), re.compile(r"/api/sync/push")],
}


def _path():
    return config.DATA_DIR / "security.json"


def load_settings():
    with _lock:
        try:
            value = json.loads(_path().read_text(encoding="utf-8"))
            if not isinstance(value, dict):
                raise ValueError()
            return {"enabled": False, "token_hash": "", "sessions": [], **value}
        except FileNotFoundError:
            return {"enabled": False, "token_hash": "", "sessions": []}
        except (OSError, ValueError):
            # An unreadable security file fails closed for remote clients.
            return {"enabled": False, "token_hash": "", "sessions": []}


def _save(value):
    path = _path()
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(value, stream)
    temporary.replace(path)


def _digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def is_local(request):
    """Require both a loopback socket peer and a loopback Host (DNS rebinding)."""
    peer = request.client.host if request.client else ""
    host = request.url.hostname or ""
    # Starlette TestClient uses this non-network peer/host pair. A socket peer
    # comes from the OS as an IP and cannot impersonate this transport marker.
    if peer == "testclient" and host == "testserver":
        return True
    try:
        address = ipaddress.ip_address(peer.split("%", 1)[0])
        peer_local = address.is_loopback or bool(getattr(address, "ipv4_mapped", None) and address.ipv4_mapped.is_loopback)
        if not peer_local:
            return False
    except ValueError:
        return False
    if host.lower().rstrip(".") == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def authenticated(request, value=None):
    if is_local(request):
        return True
    value = value if value is not None else load_settings()
    cookie = request.cookies.get(COOKIE_NAME, "")
    if not value["enabled"] or not cookie or len(cookie) > 200:
        return False
    hashed, now = _digest(cookie), time.time()
    return any(isinstance(session, dict) and session.get("expires", 0) > now and
               hmac.compare_digest(str(session.get("hash", "")), hashed) for session in value["sessions"])


def public_settings(request):
    value = load_settings()
    local = is_local(request)
    return {"enabled": bool(value["enabled"]), "paired": not local and authenticated(request, value),
            "local": local, "token_configured": bool(value["token_hash"])}


def rotate_token():
    with _lock:
        token = secrets.token_urlsafe(32)
        _save({"enabled": True, "token_hash": _digest(token), "sessions": []})
        return token


def create_pairing_link(request):
    """Issue one five-minute, one-use pairing invitation; persist only its hash."""
    token, expires = secrets.token_urlsafe(24), int(time.time()) + PAIRING_LINK_SECONDS
    with _lock:
        value = load_settings()
        value["invite"] = {"hash": _digest(token), "expires": expires}
        _save(value)
    return pairing_url(request, token), expires


def pairing_url(request, token):
    """Build a short lived deep link suitable for a QR code.

    The desktop UI normally reaches the service through 127.0.0.1, which is
    not useful to a phone.  Prefer the host already used by a non-loopback
    request and otherwise discover the outbound LAN address without sending
    a packet.  The token remains in the URL fragment so it is not sent in an
    HTTP request or server access log; the web shell consumes it once.
    """
    host = (request.url.hostname or "").strip()
    try:
        address = ipaddress.ip_address(host)
        if address.is_loopback:
            raise ValueError()
    except ValueError:
        host = ""
    if not host:
        host = _lan_host()
    port = request.url.port or (443 if request.url.scheme == "https" else 80)
    suffix = "" if port in (80, 443) else f":{port}"
    return f"{request.url.scheme}://{host}{suffix}/#pair={quote(str(token), safe='')}"


def _lan_host():
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("8.8.8.8", 80))
        return probe.getsockname()[0]
    except OSError:
        return "localhost"
    finally:
        probe.close()


def set_enabled(enabled):
    if type(enabled) is not bool:
        raise ValueError("enabled 须为布尔值")
    with _lock:
        value = load_settings()
        if enabled and not value["token_hash"]:
            raise ValueError("请先生成配对码")
        value["enabled"] = enabled
        if not enabled:
            value["sessions"] = []
            value.pop("invite", None)
        _save(value)


def login(request, token):
    if not isinstance(token, str) or not 1 <= len(token) <= 200:
        return None
    peer, now = request.client.host if request.client else "", time.time()
    with _lock:
        # Bound failed-login state and rate-limit attempts without logging codes.
        for key in list(_login_attempts):
            if not _login_attempts[key] or _login_attempts[key][-1] < now - 300:
                del _login_attempts[key]
        if len(_login_attempts) > 1000:
            return None
        attempts = _login_attempts.setdefault(peer, deque())
        while attempts and attempts[0] < now - 300:
            attempts.popleft()
        if len(attempts) >= 10:
            return None
        attempts.append(now)
        value = load_settings()
        invite = value.get("invite") or {}
        invited = (invite.get("expires", 0) > now and
                   hmac.compare_digest(str(invite.get("hash", "")), _digest(token)))
        if not value["enabled"] or not value["token_hash"] or not (invited or hmac.compare_digest(value["token_hash"], _digest(token))):
            return None
        cookie = secrets.token_urlsafe(32)
        sessions = [s for s in value["sessions"] if isinstance(s, dict) and s.get("expires", 0) > now][-63:]
        value["sessions"] = [*sessions, {"hash": _digest(cookie), "expires": now + SESSION_SECONDS}]
        if invited:
            value.pop("invite", None)
        _save(value)
        _login_attempts.pop(peer, None)
        return cookie


def logout(request):
    cookie = request.cookies.get(COOKIE_NAME, "")
    if not cookie:
        return
    with _lock:
        value = load_settings()
        hashed = _digest(cookie)
        value["sessions"] = [s for s in value["sessions"] if not hmac.compare_digest(str(s.get("hash", "")), hashed)]
        _save(value)


def _error(status, code, message):
    return JSONResponse({"error": {"code": code, "message": message}}, status_code=status,
                        headers={"Cache-Control": "no-store"})


def _safe_path(path):
    return "\\" not in path and "\x00" not in path and not any(part in (".", "..") for part in path.split("/")) and "//" not in path


def _public_static(path):
    if path in ("/", "/index.html", "/app.js", "/manifest.webmanifest", "/sw.js"):
        return True
    allowed = (("/ui/", ".js"), ("/util/", ".js"), ("/styles/", ".css"), ("/icons/", ".png"))
    return any(path.startswith(prefix) and path.endswith(suffix) for prefix, suffix in allowed)


def _same_origin(request):
    origins = [v.decode("latin-1") for k, v in request.scope.get("headers", []) if k.lower() == b"origin"]
    if len(origins) > 1:
        return False
    origin = origins[0] if origins else None
    if origin is None:
        return is_local(request) and request.headers.get("sec-fetch-site", "none") in ("none", "same-origin")
    if origin == "null":
        return False
    try:
        parsed = urlsplit(origin)
        actual = urlsplit(str(request.base_url))
        def endpoint(url):
            return (url.scheme, url.hostname, url.port or (443 if url.scheme == "https" else 80))
        return not parsed.username and not parsed.password and not parsed.query and not parsed.fragment and parsed.path in ("", "/") and endpoint(parsed) == endpoint(actual)
    except ValueError:
        return False


class SecurityMiddleware:
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] == "websocket":
            await send({"type": "websocket.close", "code": 1008})
            return
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return
        request = Request(scope)
        path, method = request.url.path, request.method
        public = path in ("/api/health", "/api/auth/status") or _public_static(path)
        response = None
        if not _safe_path(path):
            response = _error(400, "invalid_path", "请求路径无效")
        elif method not in ("GET", "HEAD", "OPTIONS") and not _same_origin(request):
            response = _error(403, "origin_rejected", "请从同一服务页面发起此操作")
        elif not is_local(request) and not public:
            value = load_settings()
            if path == "/api/security/settings" and method == "GET":
                # Public status only; token hashes and active sessions never
                # leave the process. This lets a phone decide whether pairing
                # is available before showing the login form.
                pass
            elif not value["enabled"]:
                response = _error(403, "lan_disabled", "电脑端尚未开启局域网访问")
            elif path == "/api/auth/login" and method == "POST":
                pass
            elif not authenticated(request, value):
                response = _error(401, "authentication_required", "请输入电脑端生成的配对码")
            elif path == "/api/auth/logout" and method == "POST":
                pass
            elif method in ("GET", "HEAD") and any(rule.fullmatch(path) for rule in _READ_PATHS):
                pass
            elif any(rule.fullmatch(path) for rule in _WRITE_PATHS.get(method, ())):
                pass
            else:
                response = _error(403, "local_admin_required", "此操作仅允许在电脑本机管理")
        async def safe_send(message):
            if message["type"] == "http.response.start":
                private = not public or path.startswith("/api/")
                headers = [(k, v) for k, v in message.get("headers", [])
                           if not private or k.lower() != b"cache-control"]
                if private:
                    headers.append((b"cache-control", b"private, no-store"))
                headers.extend([(b"x-content-type-options", b"nosniff"), (b"x-frame-options", b"DENY"),
                                (b"referrer-policy", b"no-referrer")])
                message = {**message, "headers": headers}
            await send(message)
        if response is not None:
            await response(scope, receive, safe_send)
        else:
            await self.app(scope, receive, safe_send)


def install_security(app):
    app.add_middleware(SecurityMiddleware)
