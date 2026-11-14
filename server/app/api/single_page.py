"""One-shot, text-free whole comic page API."""
from __future__ import annotations

from fastapi import APIRouter, Body, Query
from fastapi.responses import FileResponse

from ..manga import service as s
from ..manga import single_page
from ..manga.models import MangaError
from .manga import connection, fields

router = APIRouter(prefix="/api/books/{book_id}/manga/single-page")


@router.get("")
def listing(book_id: str, limit: int = Query(20, ge=1, le=100)):
    with connection(book_id) as conn:
        rows = conn.execute("SELECT * FROM single_comic_pages WHERE book_id=? ORDER BY created_at DESC,id LIMIT ?",
                            (book_id, limit)).fetchall()
        return {"pages": [single_page.public(conn, book_id, row) for row in rows]}


@router.post("", status_code=202)
async def create(book_id: str, payload: dict = Body(...)):
    fields(payload, {"chapter", "panels", "size", "scene_id"})
    panels = payload.get("panels", single_page.DEFAULT_PANELS)
    if isinstance(panels, bool) or not isinstance(panels, int) or not single_page.MIN_PANELS <= panels <= single_page.MAX_PANELS:
        raise MangaError("分格数须为 2–6 格")
    size = payload.get("size", "portrait")
    if size not in single_page.SIZES:
        raise MangaError("页面尺寸须为 portrait、tall 或 square")
    scene_id = payload.get("scene_id")
    if scene_id is not None and (not isinstance(scene_id, str) or not 1 <= len(scene_id) <= 200):
        raise MangaError("场景标识无效")
    chapter = payload.get("chapter")
    if scene_id is None and (isinstance(chapter, bool) or not isinstance(chapter, int) or chapter < 0):
        raise MangaError("章节须为非负整数")
    with connection(book_id) as conn:
        if scene_id is not None:
            scene = conn.execute("SELECT chapter_id FROM scenes WHERE id=? AND book_id=?", (scene_id, book_id)).fetchone()
            if not scene:
                raise MangaError("场景不存在", "scene_not_found", 404)
            chapter = scene["chapter_id"]
        if conn.execute("SELECT 1 FROM chapters WHERE book_id=? AND idx=?", (book_id, chapter)).fetchone() is None:
            raise MangaError("章节不存在", "chapter_not_found", 404)
        # Fail before queueing when either model is clearly unconfigured.
        from ..ai.director import text_config
        text_config()
        single_page._config(conn, book_id, size)
        now = single_page._now()
        page_id = s.uid("single_")
        conn.execute("""INSERT INTO single_comic_pages(id,book_id,chapter,status,panels,size,created_at,updated_at)
                        VALUES(?,?,?,'queued',?,?,?,?)""", (page_id, book_id, chapter, panels, size, now, now))
        saved = single_page._row(conn, book_id, page_id)
        result = single_page.public(conn, book_id, saved)
    single_page.start_task(page_id, book_id, chapter, scene_id, panels, size)
    return {"page": result}


@router.get("/{page_id}")
def detail(book_id: str, page_id: str):
    with connection(book_id) as conn:
        row = single_page._row(conn, book_id, page_id)
        if not row:
            raise MangaError("无字漫画页不存在", "single_page_not_found", 404)
        return {"page": single_page.public(conn, book_id, row)}


@router.get("/{page_id}/image")
def image(book_id: str, page_id: str):
    with connection(book_id) as conn:
        row = single_page._row(conn, book_id, page_id)
        if not row or not row["image_path"]:
            raise MangaError("漫画图片尚未生成", "image_missing", 404)
        path = s.asset_path(row["image_path"])
        if not path.is_file():
            raise MangaError("漫画图片文件丢失", "image_missing", 404)
    return FileResponse(path, media_type="image/png", headers={"Cache-Control": "private, no-cache"})
