"""注释 / 高亮接口（契约 7.5）。

- GET    /api/books/{book_id}/annotations?chapter=&limit=200&offset=0
- POST   /api/books/{book_id}/annotations            → 201
- PATCH  /api/books/{book_id}/annotations/{id}       → 200
- DELETE /api/books/{book_id}/annotations/{id}       → 200

校验口径：chapter 在范围内、0 ≤ start < end、len(text) == end - start、
para 在章节段落范围内、color ∈ yellow|green|blue|pink；否则 400 invalid_annotation。
"""
from __future__ import annotations

import json
import secrets
import time

from fastapi import APIRouter, Query, Request
from fastapi.responses import JSONResponse

from ..db import get_conn

router = APIRouter()

COLORS = ("yellow", "green", "blue", "pink")
_DEFAULT_LIMIT = 200
_MAX_LIMIT = 500


def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code,
                        content={"error": {"code": code, "message": message}})


def _book_row(conn, book_id: str):
    return conn.execute(
        "SELECT id, format, chapter_count FROM books WHERE id=?", (book_id,)
    ).fetchone()


def _as_int(value, default=None):
    """宽松转 int；布尔/非法返回 default（用于注释坐标校验）。"""
    if isinstance(value, bool):
        return default
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str) and value.strip().lstrip("-").isdigit():
        return int(value)
    return default


async def _json_object(request: Request):
    try:
        data = await request.json()
    except Exception:
        return None
    return data if isinstance(data, dict) else None


def _annotation_to_dict(row) -> dict:
    return {
        "id": row["id"],
        "chapter": int(row["chapter"]),
        "para": int(row["para"]),
        "start_offset": int(row["start_offset"]),
        "end_offset": int(row["end_offset"]),
        "text": row["text"],
        "note": row["note"],
        "color": row["color"],
        "created_at": int(row["created_at"]),
        "updated_at": int(row["updated_at"]),
    }


def _chapter_paras(conn, book_id: str, chapter: int):
    row = conn.execute(
        "SELECT paras_json FROM chapters WHERE book_id=? AND idx=?", (book_id, chapter)
    ).fetchone()
    if row is None:
        return None
    try:
        paras = json.loads(row["paras_json"])
    except (TypeError, ValueError):
        return None
    return paras if isinstance(paras, list) else None


@router.get("/api/books/{book_id}/annotations")
def list_annotations(
    book_id: str,
    chapter: int | None = Query(default=None),
    limit: int = Query(default=_DEFAULT_LIMIT),
    offset: int = Query(default=0),
):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        size = max(1, min(_MAX_LIMIT, int(limit)))
        skip = max(0, int(offset))
        sql = "SELECT * FROM annotations WHERE book_id=?"
        args: list = [book_id]
        if chapter is not None:
            sql += " AND chapter=?"
            args.append(int(chapter))
        sql += " ORDER BY chapter ASC, para ASC, start_offset ASC LIMIT ? OFFSET ?"
        args.extend([size, skip])
        rows = conn.execute(sql, args).fetchall()
    finally:
        conn.close()
    return {"annotations": [_annotation_to_dict(row) for row in rows]}


@router.post("/api/books/{book_id}/annotations", status_code=201)
async def create_annotation(book_id: str, request: Request):
    data = await _json_object(request)
    if data is None:
        return _error(400, "invalid_annotation", "请求体必须是 JSON 对象")

    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")

        chapter = _as_int(data.get("chapter"))
        para = _as_int(data.get("para"))
        start = _as_int(data.get("start_offset"))
        end = _as_int(data.get("end_offset"))
        color = data.get("color", "yellow")
        note = data.get("note")
        text = data.get("text")

        if chapter is None or para is None or start is None or end is None:
            return _error(400, "invalid_annotation", "chapter/para/start_offset/end_offset 必须是整数")
        if chapter < 0 or chapter >= max(1, int(book["chapter_count"] or 0)):
            return _error(400, "invalid_annotation",
                          f"chapter 越界：{chapter}（共 {book['chapter_count']} 章，0-based）")
        if start < 0 or end <= start:
            return _error(400, "invalid_annotation", f"注释区间非法：start={start} end={end}（需 0 ≤ start < end）")
        if not isinstance(text, str) or len(text) != end - start:
            return _error(400, "invalid_annotation",
                          f"text 长度 {len(text) if isinstance(text, str) else 'N/A'} 与区间长度 {end - start} 不一致")
        if not isinstance(color, str) or color not in COLORS:
            return _error(400, "invalid_annotation",
                          f"color 只允许 {'|'.join(COLORS)}，收到 {color!r}")

        paras = _chapter_paras(conn, book_id, chapter)
        if paras is None:
            return _error(400, "invalid_annotation", f"章节不存在或段落数据损坏：chapter={chapter}")
        if para < 0 or para >= len(paras):
            return _error(400, "invalid_annotation",
                          f"para 越界：{para}（本章共 {len(paras)} 段，0-based）")
        para_text = paras[para] if isinstance(paras[para], str) else str(paras[para])
        if end > len(para_text):
            return _error(400, "invalid_annotation",
                          f"end_offset 超出段落长度：end={end} len(para)={len(para_text)}")

        if note is not None and not isinstance(note, str):
            note = str(note)
        annotation_id = "an_" + secrets.token_hex(4)
        now = int(time.time())
        with conn:
            conn.execute(
                "INSERT INTO annotations(id,book_id,chapter,para,start_offset,end_offset,"
                "text,note,color,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (annotation_id, book_id, chapter, para, start, end, text, note, color, now, now),
            )
        row = conn.execute("SELECT * FROM annotations WHERE id=?", (annotation_id,)).fetchone()
    finally:
        conn.close()
    return JSONResponse(status_code=201, content={"annotation": _annotation_to_dict(row)})


@router.patch("/api/books/{book_id}/annotations/{annotation_id}")
async def update_annotation(book_id: str, annotation_id: str, request: Request):
    data = await _json_object(request)
    if data is None:
        return _error(400, "invalid_annotation", "请求体必须是 JSON 对象")

    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        row = conn.execute(
            "SELECT * FROM annotations WHERE id=? AND book_id=?", (annotation_id, book_id)
        ).fetchone()
        if row is None:
            return _error(404, "annotation_not_found", f"注释不存在：{annotation_id}")

        note = row["note"]
        color = row["color"]
        if "note" in data:
            note = data["note"]
            if note is not None and not isinstance(note, str):
                note = str(note)
        if "color" in data:
            color = data["color"]
            if not isinstance(color, str) or color not in COLORS:
                return _error(400, "invalid_annotation",
                              f"color 只允许 {'|'.join(COLORS)}，收到 {color!r}")
        now = int(time.time())
        with conn:
            conn.execute(
                "UPDATE annotations SET note=?, color=?, updated_at=? WHERE id=? AND book_id=?",
                (note, color, now, annotation_id, book_id),
            )
        row = conn.execute("SELECT * FROM annotations WHERE id=?", (annotation_id,)).fetchone()
    finally:
        conn.close()
    return {"annotation": _annotation_to_dict(row)}


@router.delete("/api/books/{book_id}/annotations/{annotation_id}")
def delete_annotation(book_id: str, annotation_id: str):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        with conn:
            cur = conn.execute(
                "DELETE FROM annotations WHERE id=? AND book_id=?", (annotation_id, book_id)
            )
        if cur.rowcount == 0:
            return _error(404, "annotation_not_found", f"注释不存在：{annotation_id}")
    finally:
        conn.close()
    return {"ok": True}
