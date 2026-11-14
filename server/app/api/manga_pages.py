from __future__ import annotations

import json
import logging
import time

from fastapi import APIRouter, Body, Query
from fastapi.responses import FileResponse

from ..manga import pages as p, composition as c, service as s
from ..manga.models import MangaError
from .manga import fields

router = APIRouter(prefix="/api/books/{book_id}/manga")


@router.get("/page-panels")
def panels(book_id: str):
    with p.connection(book_id) as conn:
        result = []
        for row in conn.execute("SELECT * FROM render_jobs WHERE book_id=? AND kind='panel' AND image_path IS NOT NULL ORDER BY created_at,id", (book_id,)):
            snap = json.loads(row["snapshot_json"])
            error = None
            try:
                p.valid_snapshot(conn, row, snap)
            except (ValueError, OSError) as exc:
                error = str(exc)
            scene, panel = snap.get("scene") or {}, snap.get("panel") or {}
            summary = scene.get("summary") or panel.get("caption") or panel.get("action") or ""
            result.append({"id": row["id"], "status": "stale" if error else row["status"], "error": error,
                           "chapter": snap["chapter"], "para": snap["para"], "summary": summary,
                           "shot": snap["shot"], "url": f'/api/books/{book_id}/manga/assets/{row["id"]}'})
        return {"panels": result}


@router.get("/pages")
def list_pages(book_id: str, deleted: bool = False):
    with p.connection(book_id) as conn:
        rows = conn.execute("SELECT * FROM manga_pages WHERE book_id=? AND deleted=? ORDER BY position,id", (book_id, int(deleted))).fetchall()
        return {"pages": [p.public_page(conn, row) for row in rows], "layouts": c.LAYOUTS, "defaults": c.DEFAULTS}


@router.post("/pages", status_code=201)
def create_page(book_id: str, payload: dict = Body(...)):
    return {"page": p.create_pages(book_id, [payload])[0]}


@router.post("/pages/auto", status_code=201)
def auto_pages(book_id: str, payload: dict = Body(...)):
    return {"pages": p.auto_pages(book_id, payload)}


@router.put("/pages/order")
def reorder(book_id: str, payload: dict = Body(...)):
    fields(payload, {"order", "revisions"})
    order, revisions = payload.get("order"), payload.get("revisions")
    if not isinstance(order, list) or any(not isinstance(x, str) for x in order) or len(set(order)) != len(order) or not isinstance(revisions, dict):
        raise MangaError("页序格式无效")
    for revision in revisions.values(): c.integer(revision, 1, 2**31, "revision")
    with p.connection(book_id, True) as conn:
        current = {r["id"]: r["revision"] for r in conn.execute("SELECT id,revision FROM manga_pages WHERE book_id=? AND deleted=0", (book_id,))}
        if set(order) != set(current) or revisions != current:
            raise MangaError("漫画页列表已变化，请刷新后重排", "page_conflict", 409)
        for index, pid in enumerate(order):
            conn.execute("UPDATE manga_pages SET position=?,revision=revision+1,updated_at=? WHERE id=?", (index, int(time.time()), pid))
    return {"ok": True}


@router.patch("/pages/{page_id}")
def update_page(book_id: str, page_id: str, payload: dict = Body(...)):
    fields(payload, {"revision", "spec"})
    return {"page": p.update_page(book_id, page_id, payload.get("revision"), payload.get("spec"))}


@router.delete("/pages/{page_id}")
def remove_page(book_id: str, page_id: str, revision: int = Query(..., ge=1)):
    return visibility(book_id, page_id, revision, True)


@router.post("/pages/{page_id}/restore")
def restore_page(book_id: str, page_id: str, payload: dict = Body(...)):
    fields(payload, {"revision"})
    return visibility(book_id, page_id, payload.get("revision"), False)


def visibility(book_id, page_id, revision, deleted):
    c.integer(revision, 1, 2**31, "revision")
    with p.connection(book_id, True) as conn:
        row = p.page_row(conn, book_id, page_id, deleted=True)
        if revision != row["revision"]:
            raise MangaError("漫画页已变化，请刷新", "page_conflict", 409)
        position = row["position"] if deleted else conn.execute("SELECT COALESCE(MAX(position),-1)+1 FROM manga_pages WHERE book_id=? AND deleted=0", (book_id,)).fetchone()[0]
        conn.execute("UPDATE manga_pages SET deleted=?,position=?,revision=revision+1,updated_at=? WHERE id=?", (int(deleted), position, int(time.time()), page_id))
    return {"ok": True}


@router.get("/pages/{page_id}/image")
def image(book_id: str, page_id: str, revision: int = Query(..., ge=1)):
    with p.connection(book_id) as conn:
        row = p.page_row(conn, book_id, page_id)
        if row["revision"] != revision:
            raise MangaError("漫画页版本已变化，请刷新预览", "page_conflict", 409)
        path = s.asset_path(row["image_path"])
        if not path.is_file():
            raise MangaError("漫画页图片丢失", "image_missing", 404)
    return FileResponse(path, media_type="image/png", headers={"Cache-Control": "private, no-cache"})


@router.post("/exports", status_code=201)
def export(book_id: str):
    result = p.export_book(book_id)
    from .books import _publish_books_changed
    try:
        _publish_books_changed()
    except Exception:
        logging.getLogger("velmira.server").exception("Failed to broadcast comic export")
    return {"export": result}


@router.get("/exports")
def exports(book_id: str):
    with p.connection(book_id) as conn:
        return {"exports": [p.export_result(book_id, r["id"], r["comic_book_id"], len(json.loads(r["manifest_json"])["pages"]))
                            for r in conn.execute("SELECT * FROM manga_exports WHERE book_id=? ORDER BY created_at DESC,rowid DESC", (book_id,))]}


@router.get("/exports/{export_id}/download")
def download(book_id: str, export_id: str):
    with p.connection(book_id) as conn:
        row = conn.execute("SELECT * FROM manga_exports WHERE id=? AND book_id=?", (export_id, book_id)).fetchone()
        if not row or not s.asset_path(row["archive_path"]).is_file():
            raise MangaError("导出文件不存在", "export_not_found", 404)
        title = json.loads(row["manifest_json"])["title"]
        import re
        filename = re.sub(r'[<>:"/\\|?*\x00-\x1f]', "_", title).strip(". ")[:100] or "comic"
        return FileResponse(s.asset_path(row["archive_path"]), media_type="application/vnd.comicbook+zip", filename=filename + ".cbz")
