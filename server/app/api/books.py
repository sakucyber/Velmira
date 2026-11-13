"""书库 / 目录 / 章节 / 进度 / 书签接口（契约 4.1-4.9；v0.3 追加 8.2 导入广播）。

路由路径自带 /api 前缀；main.py include_router 时不加 prefix。
POST /api/books 限量读取原始字节流（不解析 multipart），在线程池执行导入。
导入成功（新书 / force 重建）→ events_bus.publish_books_changed(count)；
去重命中（deduped=true）不广播。
"""
from __future__ import annotations

import json
import logging
from pathlib import Path
import secrets
import time

from fastapi import APIRouter, Query, Request
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import FileResponse, JSONResponse
from starlette.requests import ClientDisconnect

from .. import config
from .. import importer
from ..core.events_bus import events_bus
from ..db import get_conn

log = logging.getLogger("velmira.server")

router = APIRouter()


# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------

def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code,
                        content={"error": {"code": code, "message": message}})


def _book_row(conn, book_id: str):
    return conn.execute("SELECT * FROM books WHERE id=?", (book_id,)).fetchone()


def _as_int(value, default: int = 0) -> int | None:
    """宽松转 int；非法返回 None（用于进度/书签校验）。"""
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str) and value.strip().lstrip("-").isdigit():
        return int(value)
    if value is None:
        return default
    return None


def _compute_percent(conn, book_id: str, chapter: int, para: int, offset: int) -> float:
    """契约 4.8 进度公式，分母为正文总字数 Σ chapters.chars（本书 482,081）。

    percent = (Σ chapters.chars[idx < chapter] + 章内已读字数) / Σ chapters.chars
    章内已读字数 = 本章 para 之前各段字数 + offset（上限为本章字数），
    因此末章最后一段（offset=0 时）≈1.0，末段读到末尾时恰为 1.0。
    M1：cbz 改用页口径 percent = (page + 1) / page_count（契约 7.4）。
    """
    book = conn.execute(
        "SELECT format, chapter_count FROM books WHERE id=?", (book_id,)
    ).fetchone()
    if book is not None and (book["format"] or "txt") == "cbz":
        count = int(book["chapter_count"] or 0)
        if count <= 0:
            return 0.0
        page = min(max(int(chapter), 0), count - 1)
        return round(min(1.0, max(0.0, (page + 1) / count)), 6)
    total = conn.execute(
        "SELECT COALESCE(SUM(chars), 0) AS total FROM chapters WHERE book_id=?", (book_id,)
    ).fetchone()["total"]
    if not total:
        return 0.0
    before = conn.execute(
        "SELECT COALESCE(SUM(chars), 0) AS before FROM chapters WHERE book_id=? AND idx<?",
        (book_id, chapter),
    ).fetchone()["before"]
    row = conn.execute(
        "SELECT chars, paras_json FROM chapters WHERE book_id=? AND idx=?", (book_id, chapter)
    ).fetchone()
    if row is None:
        return round(min(1.0, max(0.0, before / total)), 6)
    chapter_chars = row["chars"] or 0
    try:
        paras = json.loads(row["paras_json"])
    except (TypeError, ValueError):
        paras = []
    consumed = 0
    for text in paras[:max(0, para)]:
        consumed += len(text)
    consumed += max(0, offset)
    consumed = min(consumed, chapter_chars)
    return round(min(1.0, max(0.0, (before + consumed) / total)), 6)


def _progress_row_to_dict(row, default: bool = False) -> dict:
    if row is None:
        return {"chapter": 0, "para": 0, "offset": 0, "percent": 0.0, "updated_at": 0}
    return {
        "chapter": row["chapter"],
        "para": row["para"],
        "offset": row["offset"],
        "percent": row["percent"],
        "updated_at": row["updated_at"],
    }


def _bookmark_to_dict(row) -> dict:
    return {
        "id": row["id"],
        "book_id": row["book_id"],
        "chapter": row["chapter"],
        "para": row["para"],
        "offset": row["offset"],
        "label": row["label"],
        "created_at": row["created_at"],
    }


# ---------------------------------------------------------------------------
# 4.2 导入
# ---------------------------------------------------------------------------

def _publish_books_changed() -> None:
    """契约 8.2：新书导入成功 / force 重建成功后广播一次（dedupe 不广播）。

    广播失败不得影响已经成功的导入（前端还有 5s 轮询兜底）。
    """
    conn = get_conn()
    try:
        count = conn.execute("SELECT COUNT(*) AS c FROM books WHERE id NOT IN (SELECT book_id FROM library_state)").fetchone()["c"]
    finally:
        conn.close()
    revision = events_bus.publish_books_changed(int(count))
    log.info("books_changed 广播：revision=%s count=%s", revision, count)


@router.post("/api/books", status_code=201)
async def create_book(
    request: Request,
    filename: str | None = Query(default=None),
    force: int = Query(default=0),
):
    if not filename or not filename.strip():
        return _error(400, "missing_filename", "缺少 filename 查询参数")
    # 先检查明确的 Content-Length，再按分块读取。这样大文件不会先完整
    # 进入内存，且导入解析不会阻塞负责 SSE/阅读请求的事件循环。
    content_length = request.headers.get("content-length")
    if content_length:
        try:
            if int(content_length) > config.MAX_UPLOAD_BYTES:
                return _error(413, "file_too_large", "文件超过 64MB 上限")
        except (TypeError, ValueError):
            # 非法头交给分块读取处理，不能因为它阻断合法请求。
            pass
    raw_buf = bytearray()
    try:
        async for chunk in request.stream():
            if not chunk:
                continue
            if len(raw_buf) + len(chunk) > config.MAX_UPLOAD_BYTES:
                return _error(413, "file_too_large", "文件超过 64MB 上限")
            raw_buf.extend(chunk)
    except ClientDisconnect:
        return _error(400, "invalid_upload", "上传连接已断开")
    raw = bytes(raw_buf)
    del raw_buf
    try:
        result = await run_in_threadpool(_import_and_restore, raw, filename.strip(), bool(force))
    except importer.BookImportError as exc:
        return _error(exc.status_code, exc.code, exc.message)
    except Exception as exc:  # 兜底，避免把堆栈直接抛给前端
        return _error(500, "internal_error", f"导入失败：{exc}")
    return JSONResponse(status_code=200 if result["deduped"] else 201, content=result)


def _import_and_restore(raw: bytes, filename: str, force: bool) -> dict:
    result = importer.import_book(raw, filename, force=force)
    # 数据库恢复与广播也在线程池完成，避免 SQLite 忙等占住事件循环。
    conn = get_conn()
    try:
        with conn:
            restored = conn.execute("DELETE FROM library_state WHERE book_id=?", (result["book"]["id"],)).rowcount > 0
    finally:
        conn.close()
    result["restored"] = restored
    if not result["deduped"] or restored:
        try:
            _publish_books_changed()
        except Exception as exc:  # 广播失败不影响导入结果
            log.warning("books_changed 广播失败：%s", exc)
    return result


# ---------------------------------------------------------------------------
# 4.3 / 4.4 书库列表与详情
# ---------------------------------------------------------------------------

@router.get("/api/books/{book_id}/source")
def download_source(book_id: str):
    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
    finally:
        conn.close()
    if book is None:
        return _error(404, "book_not_found", "书籍不存在")
    try:
        source = Path(book["file_path"]).resolve()
        source.relative_to(config.DATA_DIR.resolve())
        if not source.is_file():
            return _error(404, "source_not_found", "原文件不存在")
    except (ValueError, OSError):
        return _error(404, "source_not_found", "原文件不存在")
    fmt = book["format"] if book["format"] in ("txt", "epub", "cbz") else "bin"
    return FileResponse(source, media_type="application/octet-stream",
                        filename=f"{book['title']}.{fmt}", headers={"Cache-Control": "no-store"})


@router.get("/api/books")
def list_books(archived: bool = False):
    conn = get_conn()
    try:
        rows = conn.execute(
            """
            SELECT b.id, b.title, b.format, b.total_chars, b.chapter_count, b.created_at,
                   p.chapter AS last_chapter, p.percent AS percent, p.updated_at AS last_read_at,
                   s.archived_at
            FROM books b
            LEFT JOIN progress p ON p.book_id = b.id
            LEFT JOIN library_state s ON s.book_id = b.id
            WHERE (s.book_id IS NOT NULL) = ?
            ORDER BY b.created_at DESC, b.id ASC
            """, (int(archived),)
        ).fetchall()
    finally:
        conn.close()
    books = []
    for r in rows:
        fmt = r["format"] or "txt"
        chapter_count = int(r["chapter_count"] or 0)
        if r["percent"] is not None:
            percent = float(r["percent"])
        elif fmt == "cbz" and chapter_count > 0:
            # 契约 7.4：无进度记录的 cbz 也要遵守 (page+1)/page_count（未读 chapter=0 → 1/count）
            percent = round(min(1.0, 1.0 / chapter_count), 6)
        else:
            percent = 0.0
        item = {
            "id": r["id"],
            "title": r["title"],
            # M1：格式徽标（契约 7.3），cbz 追加 page_count
            "format": fmt,
            "total_chars": r["total_chars"],
            "chapter_count": r["chapter_count"],
            "created_at": r["created_at"],
            "last_chapter": r["last_chapter"] if r["last_chapter"] is not None else 0,
            "percent": percent,
            "has_progress": r["last_read_at"] is not None,
            "last_read_at": r["last_read_at"] or 0,
            "archived_at": r["archived_at"],
        }
        if fmt == "cbz":
            item["page_count"] = chapter_count
        books.append(item)
    return {"books": books}


@router.patch("/api/books/{book_id}")
async def update_book(book_id: str, request: Request):
    try:
        body = await request.json()
    except (ValueError, TypeError):
        return _error(400, "invalid_json", "需要 JSON 对象")
    if not isinstance(body, dict) or not body or set(body) - {"title", "archived"}:
        return _error(400, "invalid_fields", "仅支持 title 和 archived")
    title = body.get("title")
    if "title" in body and (not isinstance(title, str) or not title.strip() or len(title.strip()) > 200):
        return _error(400, "invalid_title", "书名须为 1–200 个字符")
    if "archived" in body and not isinstance(body["archived"], bool):
        return _error(400, "invalid_archived", "archived 必须为布尔值")
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", "书籍不存在")
        with conn:
            if "title" in body:
                conn.execute("UPDATE books SET title=? WHERE id=?", (title.strip(), book_id))
            if "archived" in body:
                if body["archived"]:
                    conn.execute("INSERT INTO library_state(book_id,archived_at) VALUES(?,?) ON CONFLICT(book_id) DO NOTHING", (book_id, int(time.time())))
                else:
                    conn.execute("DELETE FROM library_state WHERE book_id=?", (book_id,))
        book = importer.book_to_dict(_book_row(conn, book_id))
    finally:
        conn.close()
    try:
        _publish_books_changed()
    except Exception as exc:
        log.warning("books_changed 广播失败：%s", exc)
    return {"book": book}


@router.get("/api/books/{book_id}")
def get_book(book_id: str):
    conn = get_conn()
    try:
        row = _book_row(conn, book_id)
        if row is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        book = importer.book_to_dict(row)
        from ..manga.pages import edition_links
        book["reading_links"] = edition_links(conn, book_id)
        if (row["format"] or "txt") == "cbz":
            # 契约 7.3：cbz 详情给 page_count（book_to_dict 已含，这里兜底）
            book["page_count"] = int(row["chapter_count"] or 0)
        else:
            # 契约 4.4：txt/epub 详情给 para_count（原有）
            book["para_count"] = importer.paragraph_count_for(row)
    finally:
        conn.close()
    return {"book": book}


# ---------------------------------------------------------------------------
# 4.5 / 4.6 目录与章节正文
# ---------------------------------------------------------------------------

@router.get("/api/books/{book_id}/toc")
def get_toc(book_id: str):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        rows = conn.execute(
            "SELECT idx, title, chars FROM chapters WHERE book_id=? ORDER BY idx", (book_id,)
        ).fetchall()
    finally:
        conn.close()
    return {
        "book_id": book_id,
        "chapters": [{"idx": r["idx"], "title": r["title"], "chars": r["chars"]} for r in rows],
    }


@router.get("/api/books/{book_id}/chapters/{idx}")
def get_chapter(book_id: str, idx: int):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        row = conn.execute(
            "SELECT idx, title, chars, paras_json FROM chapters WHERE book_id=? AND idx=?",
            (book_id, idx),
        ).fetchone()
    finally:
        conn.close()
    if row is None:
        return _error(404, "chapter_not_found", f"章节不存在：{idx}")
    return {
        "book_id": book_id,
        "idx": row["idx"],
        "title": row["title"],
        "chars": row["chars"],
        "paragraphs": json.loads(row["paras_json"]),
    }


# ---------------------------------------------------------------------------
# 4.7 / 4.8 进度
# ---------------------------------------------------------------------------

@router.get("/api/books/{book_id}/progress")
def get_progress(book_id: str):
    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        row = conn.execute("SELECT * FROM progress WHERE book_id=?", (book_id,)).fetchone()
        progress = _progress_row_to_dict(row)
        if row is not None:
            # 读取时按当前公式重算，保证与写入一致（契约 4.8）
            progress["percent"] = _compute_percent(
                conn, book_id, row["chapter"], row["para"], row["offset"]
            )
        elif (book["format"] or "txt") == "cbz":
            # 契约 7.4：无进度记录的默认值也必须遵守页公式。
            # 默认 chapter=0 → percent = 1/page_count（5 页书 0.2），不能返回 0.0。
            count = int(book["chapter_count"] or 0)
            progress["percent"] = round(min(1.0, 1.0 / count), 6) if count > 0 else 0.0
    finally:
        conn.close()
    return {"progress": progress}


@router.put("/api/books/{book_id}/progress")
async def put_progress(book_id: str, request: Request):
    try:
        data = await request.json()
    except Exception:
        return _error(400, "invalid_locator", "请求体必须是 JSON 对象")
    if not isinstance(data, dict):
        return _error(400, "invalid_locator", "请求体必须是 JSON 对象")

    chapter = _as_int(data.get("chapter"), 0)
    para = _as_int(data.get("para"), 0)
    offset = _as_int(data.get("offset"), 0)
    if chapter is None or para is None or offset is None or chapter < 0 or para < 0 or offset < 0:
        return _error(400, "invalid_locator", "chapter/para/offset 必须是非负整数")

    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        if chapter >= max(1, book["chapter_count"]):
            return _error(400, "invalid_locator",
                          f"chapter 越界：{chapter}（共 {book['chapter_count']} 章，0-based）")
        percent = _compute_percent(conn, book_id, chapter, para, offset)
        now = int(time.time())
        with conn:
            conn.execute(
                """
                INSERT INTO progress(book_id, chapter, para, offset, percent, updated_at)
                VALUES(?,?,?,?,?,?)
                ON CONFLICT(book_id) DO UPDATE SET
                    chapter=excluded.chapter,
                    para=excluded.para,
                    offset=excluded.offset,
                    percent=excluded.percent,
                    updated_at=excluded.updated_at
                """,
                (book_id, chapter, para, offset, percent, now),
            )
        row = conn.execute("SELECT * FROM progress WHERE book_id=?", (book_id,)).fetchone()
        progress = _progress_row_to_dict(row)
        if row is not None:
            progress["percent"] = _compute_percent(
                conn, book_id, row["chapter"], row["para"], row["offset"]
            )
    finally:
        conn.close()
    return {"progress": progress}


# ---------------------------------------------------------------------------
# 4.9 书签
# ---------------------------------------------------------------------------

@router.get("/api/books/{book_id}/bookmarks")
def list_bookmarks(book_id: str):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        rows = conn.execute(
            "SELECT * FROM bookmarks WHERE book_id=? ORDER BY created_at DESC, id ASC",
            (book_id,),
        ).fetchall()
    finally:
        conn.close()
    return {"bookmarks": [_bookmark_to_dict(r) for r in rows]}


@router.post("/api/books/{book_id}/bookmarks", status_code=201)
async def create_bookmark(book_id: str, request: Request):
    try:
        data = await request.json()
    except Exception:
        return _error(400, "invalid_locator", "请求体必须是 JSON 对象")
    if not isinstance(data, dict):
        return _error(400, "invalid_locator", "请求体必须是 JSON 对象")

    chapter = _as_int(data.get("chapter"), 0)
    para = _as_int(data.get("para"), 0)
    offset = _as_int(data.get("offset"), 0)
    if chapter is None or para is None or offset is None or chapter < 0 or para < 0 or offset < 0:
        return _error(400, "invalid_locator", "chapter/para/offset 必须是非负整数")

    conn = get_conn()
    try:
        book = _book_row(conn, book_id)
        if book is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        if chapter >= max(1, book["chapter_count"]):
            return _error(400, "invalid_locator",
                          f"chapter 越界：{chapter}（共 {book['chapter_count']} 章，0-based）")
        label = data.get("label")
        if label is not None and not isinstance(label, str):
            label = str(label)
        bookmark_id = "bm_" + secrets.token_hex(8)
        now = int(time.time())
        with conn:
            conn.execute(
                """
                INSERT INTO bookmarks(id,book_id,chapter,para,offset,label,created_at)
                VALUES(?,?,?,?,?,?,?)
                ON CONFLICT(book_id, chapter, para) DO NOTHING
                """,
                (bookmark_id, book_id, chapter, para, offset, label, now),
            )
        row = conn.execute(
            "SELECT * FROM bookmarks WHERE book_id=? AND chapter=? AND para=?",
            (book_id, chapter, para),
        ).fetchone()
    finally:
        conn.close()
    return JSONResponse(status_code=201, content={"bookmark": _bookmark_to_dict(row)})


@router.delete("/api/books/{book_id}/bookmarks/{bookmark_id}")
def delete_bookmark(book_id: str, bookmark_id: str):
    conn = get_conn()
    try:
        if _book_row(conn, book_id) is None:
            return _error(404, "book_not_found", f"书籍不存在：{book_id}")
        with conn:
            cur = conn.execute(
                "DELETE FROM bookmarks WHERE id=? AND book_id=?", (bookmark_id, book_id)
            )
        if cur.rowcount == 0:
            return _error(404, "bookmark_not_found", f"书签不存在：{bookmark_id}")
    finally:
        conn.close()
    return {"ok": True}
