"""检索 API：契约 4.12（GET /api/books/{id}/search?q=...&limit=20）。

索引建立/查询实现全在 server/app/pipeline/search_index.py（sqlite3 + FTS5 纯函数），
本模块只做参数校验、错误形状与计时。
"""
from __future__ import annotations

import time

from fastapi import APIRouter
from fastapi.responses import JSONResponse

from .. import db
from ..pipeline import search_index

router = APIRouter()

_DEFAULT_LIMIT = 20
_MAX_LIMIT = 100


def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code, content={"error": {"code": code, "message": message}})


def _book_exists(conn, book_id: str) -> bool:
    return conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone() is not None


@router.get("/api/books/{book_id}/search")
def search_book(book_id: str, q: str = "", limit: int = _DEFAULT_LIMIT):
    started = time.perf_counter()
    query = (q or "").strip()
    if not query:
        return _error(400, "empty_query", "查询词不能为空（q 至少 1 个字符）")

    try:
        limit_value = int(limit)
    except (TypeError, ValueError):
        limit_value = _DEFAULT_LIMIT
    limit_value = max(1, min(_MAX_LIMIT, limit_value))

    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        result = search_index.search(conn, book_id, query, limit_value)
        return {
            "book_id": book_id,
            "q": query,
            "elapsed_ms": int((time.perf_counter() - started) * 1000),
            "hits": result["hits"],
            "index": result["index"],
        }
    finally:
        conn.close()
