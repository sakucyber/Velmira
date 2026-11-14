"""Device registration and resumable local-first synchronization endpoints."""
from fastapi import APIRouter, Body, Query, Request
from fastapi.responses import JSONResponse
import time

from .. import db, security, sync

router = APIRouter()


def _error(exc):
    return JSONResponse({"error": {"code": exc.code, "message": str(exc)}}, status_code=exc.status)


def _session_hash(request):
    if security.is_local(request): return None
    return security._digest(request.cookies.get(security.COOKIE_NAME, ""))


@router.post("/api/sync/device")
def register(request: Request, payload: dict = Body(...)):
    try:
        sessions = {item.get("hash") for item in security.load_settings()["sessions"] if item.get("expires", 0) > time.time()}
        return sync.register_device(payload, _session_hash(request), sessions)
    except sync.SyncError as exc: return _error(exc)


@router.get("/api/sync/devices")
def devices():
    conn = db.get_conn()
    try: rows = [dict(row) for row in conn.execute("SELECT id,label,created_at,last_seen,revoked_at FROM sync_devices ORDER BY last_seen DESC").fetchall()]
    finally: conn.close()
    return {"devices": rows}


@router.delete("/api/sync/devices/{device_id}")
def revoke_device(request: Request, device_id: str):
    if not security.is_local(request):
        return _error(sync.SyncError("仅本机可撤销同步设备", "local_admin_required", 403))
    if device_id == "server":
        return _error(sync.SyncError("不能撤销本机服务"))
    conn = db.get_conn()
    try:
        with conn:
            owner = conn.execute("SELECT session_hash FROM sync_device_sessions WHERE device_id=?", (device_id,)).fetchone()
            changed = conn.execute("UPDATE sync_devices SET revoked_at=? WHERE id=? AND revoked_at IS NULL", (int(time.time()), device_id)).rowcount
    finally: conn.close()
    if not changed: return _error(sync.SyncError("设备不存在或已撤销", "device_not_found", 404))
    if owner:
        # Local devices have no session row; revoking them never affects LAN
        # pairings. A remote device loses only the cookie that registered it.
        with security._lock:
            value = security.load_settings()
            value["sessions"] = [item for item in value["sessions"] if item.get("hash") != owner[0]]
            security._save(value)
    return {"ok": True}


@router.post("/api/sync/push")
def push(request: Request, payload: dict = Body(...)):
    try:
        if set(payload) != {"device_id", "operations"}: raise sync.SyncError("请提供 device_id 和 operations")
        if not isinstance(payload.get("operations"), list): raise sync.SyncError("operations 必须是数组")
        sync.check_device_session(payload["device_id"], _session_hash(request))
        if not security.is_local(request) and any(isinstance(item, dict) and item.get("entity") == "memory" for item in payload["operations"]):
            raise sync.SyncError("长期记忆仅允许本机管理", "local_admin_required", 403)
        return sync.push(payload["device_id"], payload["operations"])
    except sync.SyncError as exc: return _error(exc)


@router.get("/api/sync/pull")
def pull(request: Request, device_id: str, cursor: int = Query(default=0), limit: int = Query(default=100)):
    try:
        sync.check_device_session(device_id, _session_hash(request))
        return sync.pull(device_id, cursor, limit, include_memory=security.is_local(request))
    except sync.SyncError as exc: return _error(exc)


@router.get("/api/sync/snapshot")
def get_snapshot(book_id: str | None = None, offset: int = Query(default=0), limit: int = Query(default=500)):
    try: return sync.snapshot(book_id, offset, limit)
    except sync.SyncError as exc: return _error(exc)


@router.get("/api/sync/books/{book_id}/manifest")
def book_manifest(book_id: str):
    """Small immutable-resource index for a device's download queue."""
    conn = db.get_conn()
    try:
        book = conn.execute("SELECT id,title,format,sha256,chapter_count,total_chars FROM books WHERE id=?", (book_id,)).fetchone()
        if not book: return _error(sync.SyncError("书籍不存在", "book_not_found", 404))
        resources = []
        for row in conn.execute("SELECT idx,chars FROM chapters WHERE book_id=? ORDER BY idx", (book_id,)):
            resources.append({"kind": "chapter", "idx": row["idx"], "chars": row["chars"], "url": f"/api/books/{book_id}/chapters/{row['idx']}"})
        for row in conn.execute("SELECT idx,bytes FROM book_pages WHERE book_id=? ORDER BY idx", (book_id,)):
            resources.append({"kind": "comic_page", "idx": row["idx"], "bytes": row["bytes"], "url": f"/media/{book_id}/{row['idx']}"})
        for row in conn.execute("SELECT id,bytes FROM epub_assets WHERE book_id=? ORDER BY id", (book_id,)):
            resources.append({"kind": "epub_asset", "id": row["id"], "bytes": row["bytes"], "url": f"/api/books/{book_id}/epub/assets/{row['id']}"})
    finally: conn.close()
    return {"book": dict(book), "resources": resources, "source_url": f"/api/books/{book_id}/source"}
