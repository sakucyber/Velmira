"""Automatic comics remain drafts; they never claim human approval or replace M4 editions."""
import json

from fastapi import APIRouter, Body, Query
from fastapi.responses import FileResponse

from ..manga import automatic as a, pages, service as s
from ..manga.models import MangaError
from .manga import connection, fields

router = APIRouter(prefix="/api/books/{book_id}/manga/auto")


@router.get("")
def status(book_id: str):
    with connection(book_id) as conn:
        return {"auto": a.overview(conn, book_id)}


@router.patch("")
def toggle(book_id: str, payload: dict = Body(...)):
    fields(payload, {"enabled", "chapter"})
    if not isinstance(payload.get("enabled"), bool):
        raise MangaError("enabled 须为布尔值")
    chapter = payload.get("chapter", 0)
    if isinstance(chapter, bool) or not isinstance(chapter, int) or chapter < 0:
        raise MangaError("chapter 须为非负整数")
    with connection(book_id) as conn:
        return {"auto": a.toggle(conn, book_id, payload["enabled"], chapter)}


def check_draft(conn, book_id, row):
    pending = 0
    for source in json.loads(row["sources_json"]):
        if source["job_id"]:
            job = conn.execute("SELECT * FROM render_jobs WHERE id=? AND book_id=?", (source["job_id"], book_id)).fetchone()
            if not job:
                raise MangaError("分镜已不存在")
            if a.job_ready(conn, job["id"]) is None:
                raise MangaError("分镜正在重新生成")
            pending += job["status"] == "needs_review"
        path = s.asset_path(source["image_path"])
        if not path.is_file() or pages.file_hash(path) != source["image_hash"]:
            raise MangaError("分镜图片丢失或发生变化")
    target = s.asset_path(row["image_path"])
    if not target.is_file() or pages.file_hash(target) != row["image_hash"]:
        raise MangaError("漫画页图片丢失或发生变化")
    return pending


@router.get("/runs")
def runs(book_id: str):
    with pages.connection(book_id) as conn:
        return {"runs": [dict(r) for r in conn.execute("SELECT run_id,MIN(chapter) AS start_chapter,COUNT(*) AS pages,MIN(created_at) AS created_at FROM auto_manga_pages WHERE book_id=? GROUP BY run_id ORDER BY created_at DESC,run_id LIMIT 100", (book_id,))]}


@router.get("/pages")
def list_pages(book_id: str, offset: int = Query(0, ge=0), limit: int = Query(12, ge=1, le=30), run_id: str | None = None, chapter: int | None = Query(None, ge=0)):
    with pages.connection(book_id) as conn:
        current = a.row(conn, book_id)
        run_id = run_id or (current["run_id"] if current else "")
        where = "book_id=? AND run_id=?" + (" AND chapter=?" if chapter is not None else "")
        args = (book_id, run_id, chapter) if chapter is not None else (book_id, run_id)
        total = conn.execute("SELECT COUNT(*) FROM auto_manga_pages WHERE " + where, args).fetchone()[0]
        result = []
        for row in conn.execute("SELECT * FROM auto_manga_pages WHERE " + where + " ORDER BY ordinal LIMIT ? OFFSET ?", (*args, limit, offset)):
            error, pending = None, 0
            try:
                pending = check_draft(conn, book_id, row)
            except (ValueError, OSError) as exc:
                error = str(exc)
            result.append({"id": row["id"], "chapter": row["chapter"], "ordinal": row["ordinal"],
                           "title": json.loads(row["spec_json"])["title"], "status": "stale" if error else "draft",
                           "pending_review": pending, "error": error,
                           "url": f"/api/books/{book_id}/manga/auto/pages/{row['id']}/image"})
        return {"pages": result, "total": total, "has_more": offset + len(result) < total}


@router.get("/logs")
def logs(book_id: str, after: int = Query(0, ge=0), limit: int = Query(100, ge=1, le=200), run_id: str | None = None):
    with connection(book_id) as conn:
        job = a.row(conn, book_id)
        current = job["run_id"] if job else ""
        selected = run_id or current
        runs = [selected]
        if selected == current:
            analysis = conn.execute("SELECT run_id FROM book_analysis WHERE book_id=?", (book_id,)).fetchone()
            if analysis:
                runs.append(analysis[0])
        marks = ",".join("?" for _ in runs)
        query = f"SELECT id,stage,level,message,created_at FROM task_logs WHERE book_id=? AND run_id IN ({marks}) AND id>? ORDER BY id"
        # First load shows the latest bounded window; later loads follow its cursor.
        if not after:
            query += " DESC LIMIT ?"
        else:
            query += " LIMIT ?"
        entries = [dict(r) for r in conn.execute(query, (book_id, *runs, after, limit))]
        if not after:
            entries.reverse()
        return {"logs": entries, "run_id": selected, "cursor": entries[-1]["id"] if entries else after}


@router.get("/pages/{page_id}/image")
def image(book_id: str, page_id: str):
    with pages.connection(book_id) as conn:
        row = conn.execute("SELECT * FROM auto_manga_pages WHERE id=? AND book_id=?", (page_id, book_id)).fetchone()
        if not row:
            raise MangaError("自动漫画页不存在", "auto_page_not_found", 404)
        path = s.asset_path(row["image_path"])
        if not path.is_file():
            raise MangaError("漫画页图片丢失", "image_missing", 404)
    return FileResponse(path, media_type="image/png", headers={"Cache-Control": "private, no-cache"})
