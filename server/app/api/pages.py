"""漫画分页与媒体接口（契约 7.4）。

- GET /api/books/{book_id}/pages/{idx}
- GET /api/books/{book_id}/pages?from=&limit=
- GET /media/{book_id}/{idx}（图片字节 + immutable 缓存头）

路径自带 /api 前缀；main.py include_router 时不加 prefix。
"""
from __future__ import annotations

from fastapi import APIRouter, Query
from fastapi.responses import FileResponse, JSONResponse

from .. import config
from ..db import get_conn

router = APIRouter()

_DEFAULT_LIMIT = 12
_MAX_LIMIT = 50

_MEDIA_TYPES = {
    "jpg": "image/jpeg",
    "jpeg": "image/jpeg",
    "png": "image/png",
    "webp": "image/webp",
    "gif": "image/gif",
}
_IMMUTABLE_CACHE = "public, max-age=31536000, immutable"


def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code,
                        content={"error": {"code": code, "message": message}})


def _book_row(conn, book_id: str):
    return conn.execute(
        "SELECT id, format, chapter_count FROM books WHERE id=?", (book_id,)
    ).fetchone()


def _page_count(conn, book_id: str, book) -> int:
    row = conn.execute(
        "SELECT COUNT(*) AS c FROM book_pages WHERE book_id=?", (book_id,)
    ).fetchone()
    if row is not None and row["c"]:
        return int(row["c"])
    return int(book["chapter_count"] or 0)


def _page_row(conn, book_id: str, idx: int):
    return conn.execute(
        "SELECT idx, path, width, height, bytes FROM book_pages WHERE book_id=? AND idx=?",
        (book_id, idx),
    ).fetchone()


def _page_to_dict(book_id: str, row) -> dict:
    return {
        "idx": int(row["idx"]),
        "url": f"/media/{book_id}/{int(row['idx'])}",
        "width": int(row["width"] or 0),
        "height": int(row["height"] or 0),
    }


def _resolve_media_path(rel_path: str):
    """book_pages.path（相对 DATA_DIR）→ 绝对路径；禁止逃出 DATA_DIR。"""
    root = config.DATA_DIR.resolve()
    target = (config.DATA_DIR / rel_path).resolve()
    try:
        target.relative_to(root)
    except ValueError:
        return None
    return target


# ---------------------------------------------------------------------------
# 分页元数据
# ---------------------------------------------------------------------------

@router.get("/api/books/{book_id}/pages/{idx}")
def get_page(book_id: str, idx: int):
    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        if (book["format"] or "txt") != "cbz":
            return _error(400, "not_comic", f"不是漫画书（format={book['format']}）")
        count = _page_count(conn, book_id, book)
        row = _page_row(conn, book_id, idx)
        if row is None:
            return _error(404, "page_not_found", f"页码不存在：{idx}（共 {count} 页，0-based）")
        page = _page_to_dict(book_id, row)
    finally:
        conn.close()
    return {
        "book_id": book_id,
        "idx": page["idx"],
        "count": count,
        "url": page["url"],
        "width": page["width"],
        "height": page["height"],
    }


@router.get("/api/books/{book_id}/pages")
def list_pages(
    book_id: str,
    from_: int = Query(default=0, alias="from"),
    limit: int = Query(default=_DEFAULT_LIMIT),
):
    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        if (book["format"] or "txt") != "cbz":
            return _error(400, "not_comic", f"不是漫画书（format={book['format']}）")
        start = max(0, int(from_))
        size = max(1, min(_MAX_LIMIT, int(limit)))
        count = _page_count(conn, book_id, book)
        rows = conn.execute(
            "SELECT idx, path, width, height, bytes FROM book_pages"
            " WHERE book_id=? AND idx>=? ORDER BY idx LIMIT ?",
            (book_id, start, size),
        ).fetchall()
    finally:
        conn.close()
    return {
        "pages": [_page_to_dict(book_id, row) for row in rows],
        "count": count,
        "from": start,
    }


# ---------------------------------------------------------------------------
# 图片字节（immutable 缓存）
# ---------------------------------------------------------------------------

@router.get("/media/{book_id}/{idx}")
def get_media(book_id: str, idx: int):
    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "page_not_found", f"书籍/页码不存在：{book_id}/{idx}")
        if (book["format"] or "txt") != "cbz":
            return _error(400, "not_comic", f"不是漫画书（format={book['format']}）")
        row = _page_row(conn, book_id, idx)
    finally:
        conn.close()

    if row is None:
        return _error(404, "page_not_found", f"页码不存在：{idx}")
    path = _resolve_media_path(row["path"])
    if path is None or not path.is_file():
        return _error(404, "page_not_found", f"图片文件不存在：{row['path']}")
    ext = path.suffix.lower().lstrip(".")
    return FileResponse(
        str(path),
        media_type=_MEDIA_TYPES.get(ext, "application/octet-stream"),
        headers={"Cache-Control": _IMMUTABLE_CACHE},
    )
