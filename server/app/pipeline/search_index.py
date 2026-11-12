"""FTS5(trigram) 检索索引（sqlite3，不碰 FastAPI / 不碰 HTTP）。

契约 4.12 实测口径（sqlite 3.45.1）：
- q >= 3 字：para_fts MATCH + bm25 排序（bm25 越小越相关，返回负值）
- q == 2 字：trigram MATCH 不可用 -> para_fts 的 LIKE 查询（真实命中，按 chapter/para 排序）
- q == 1 字：chapters 表 LIKE 兜底，响应置 degraded=true，不报错
- FTS5 不可用（建表失败）时全部走 chapters LIKE，并持久化 degraded=1

索引按需增量：search_index_state.chapters_indexed 记录已索引章数，
首次查询/新章导入后补齐；单本 234 章建索引 < 2s。
snippet 先 HTML 转义再把命中词包 <b>，前端可安全 innerHTML。
"""
from __future__ import annotations

import html
import json
import sqlite3
import time
from typing import Sequence

FTS_TABLE = "para_fts"
STATE_TABLE = "search_index_state"
DEFAULT_LIMIT = 20
MAX_LIMIT = 100
SNIPPET_RADIUS = 30
MIN_MATCH_CHARS = 3  # trigram MATCH 至少 3 字符

_DDL_STATE = """
CREATE TABLE IF NOT EXISTS search_index_state (
  book_id TEXT PRIMARY KEY,
  chapters_indexed INTEGER NOT NULL DEFAULT 0,
  degraded INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
)
"""

_DDL_FTS = """
CREATE VIRTUAL TABLE IF NOT EXISTS para_fts USING fts5(
  text,
  book_id UNINDEXED,
  chapter_idx UNINDEXED,
  para_idx UNINDEXED,
  tokenize='trigram'
)
"""


# ---------------------------------------------------------------- schema

def ensure_schema(conn: sqlite3.Connection) -> None:
    """幂等建索引状态表与 FTS 表（schema.sql 已建则为 no-op）。"""
    conn.execute(_DDL_STATE)
    try:
        conn.execute(_DDL_FTS)
    except sqlite3.OperationalError:
        # 极老版本 sqlite 无 trigram/FTS5：查询层自动降级 LIKE
        pass
    conn.commit()


def has_fts(conn: sqlite3.Connection) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (FTS_TABLE,)
    ).fetchone()
    return row is not None


def get_state(conn: sqlite3.Connection, book_id: str) -> dict:
    row = conn.execute(
        f"SELECT chapters_indexed, degraded FROM {STATE_TABLE} WHERE book_id=?", (book_id,)
    ).fetchone()
    if row is None:
        return {"chapters_indexed": 0, "degraded": 0}
    return {"chapters_indexed": int(row[0]), "degraded": int(row[1])}


def _set_degraded(conn: sqlite3.Connection, book_id: str, degraded: int) -> None:
    conn.execute(
        f"""INSERT INTO {STATE_TABLE}(book_id, chapters_indexed, degraded, updated_at)
            VALUES (?, 0, ?, ?)
            ON CONFLICT(book_id) DO UPDATE SET degraded=excluded.degraded, updated_at=excluded.updated_at""",
        (book_id, int(degraded), int(time.time())),
    )
    conn.commit()


# ---------------------------------------------------------------- index build

def ensure_book_index(conn: sqlite3.Connection, book_id: str, force: bool = False) -> dict:
    """按需增量建/补索引，返回 {chapters_indexed, degraded}。"""
    ensure_schema(conn)
    state = get_state(conn, book_id)
    if not has_fts(conn):
        _set_degraded(conn, book_id, 1)
        return {"chapters_indexed": state["chapters_indexed"], "degraded": 1}

    chapters = conn.execute(
        "SELECT idx, paras_json FROM chapters WHERE book_id=? ORDER BY idx", (book_id,)
    ).fetchall()
    if not chapters:
        return {"chapters_indexed": state["chapters_indexed"], "degraded": state["degraded"]}

    max_idx = max(int(row[0]) for row in chapters)
    start = 0 if force else state["chapters_indexed"]
    if state["chapters_indexed"] > max_idx + 1:
        # 书被重导入/换过（章数变少），状态失效 -> 全量重建
        force = True
        start = 0
    pending = [row for row in chapters if force or int(row[0]) >= start]
    if not pending:
        return {"chapters_indexed": state["chapters_indexed"], "degraded": state["degraded"]}

    if force:
        conn.execute(f"DELETE FROM {FTS_TABLE} WHERE book_id=?", (book_id,))
        start = 0

    rows: list[tuple[str, str, int, int]] = []
    indexed_max = start - 1
    for row in pending:
        chapter_idx = int(row[0])
        try:
            paras = json.loads(row[1])
        except (TypeError, ValueError):
            paras = []
        if not isinstance(paras, list):
            paras = []
        for para_idx, para in enumerate(paras):
            text = str(para)
            if text:
                rows.append((text, book_id, chapter_idx, para_idx))
        indexed_max = max(indexed_max, chapter_idx)

    if rows:
        conn.executemany(
            f"INSERT INTO {FTS_TABLE}(text, book_id, chapter_idx, para_idx) VALUES (?,?,?,?)",
            rows,
        )
    conn.execute(
        f"""INSERT INTO {STATE_TABLE}(book_id, chapters_indexed, degraded, updated_at)
            VALUES (?, ?, 0, ?)
            ON CONFLICT(book_id) DO UPDATE SET
              chapters_indexed=excluded.chapters_indexed,
              updated_at=excluded.updated_at""",
        (book_id, max(0, indexed_max + 1), int(time.time())),
    )
    conn.commit()
    state = get_state(conn, book_id)
    return {"chapters_indexed": state["chapters_indexed"], "degraded": state["degraded"]}


# ---------------------------------------------------------------- snippet

def escape_html(text: str) -> str:
    return html.escape("" if text is None else str(text), quote=True)


def make_snippet(text: str, query: str, radius: int = SNIPPET_RADIUS) -> str:
    """HTML 转义后把命中词（全部出现位置）包 <b>，两端截断加 ……

    不返回未转义原文，前端可安全 innerHTML。
    """
    text = "" if text is None else str(text)
    query = "" if query is None else str(query)
    if not text:
        return ""
    pos = text.find(query) if query else -1
    if pos < 0:
        window = text[: radius * 2]
        return escape_html(window) + ("……" if len(text) > len(window) else "")
    start = max(0, pos - radius)
    end = min(len(text), pos + len(query) + radius)
    window = text[start:end]
    escaped = escape_html(window)
    escaped_query = escape_html(query)
    if escaped_query:
        escaped = escaped.replace(escaped_query, f"<b>{escaped_query}</b>")
    prefix = "……" if start > 0 else ""
    suffix = "……" if end < len(text) else ""
    return prefix + escaped + suffix


def _hit(chapter: int, para: int, text: str, query: str, score: float) -> dict:
    return {
        "chapter": int(chapter),
        "para": int(para),
        "text": str(text),
        "snippet": make_snippet(text, query),
        "score": float(score),
    }


def _clamp_limit(limit: int) -> int:
    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = DEFAULT_LIMIT
    return max(1, min(MAX_LIMIT, limit))


# ---------------------------------------------------------------- search impl

def _search_match(conn: sqlite3.Connection, book_id: str, query: str, limit: int) -> list[dict]:
    # 整词转成 FTS5 短语：双引号包裹，内部引号双写，避免语法注入/报错
    fts_query = '"' + query.replace('"', '""') + '"'
    rows = conn.execute(
        f"""SELECT chapter_idx, para_idx, text, bm25({FTS_TABLE}) AS rank
            FROM {FTS_TABLE}
            WHERE {FTS_TABLE} MATCH ? AND book_id = ?
            ORDER BY rank ASC, chapter_idx ASC, para_idx ASC
            LIMIT ?""",
        (fts_query, book_id, limit),
    ).fetchall()
    return [_hit(r[0], r[1], r[2], query, round(float(r[3]), 3)) for r in rows]


def _escape_like(term: str) -> str:
    return term.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


def _search_like_fts(conn: sqlite3.Connection, book_id: str, query: str, limit: int) -> list[dict]:
    """2 字查询：trigram MATCH 不可用，扫 para_fts 的 text 列做 LIKE（真实命中）。"""
    pattern = f"%{_escape_like(query)}%"
    rows = conn.execute(
        f"""SELECT chapter_idx, para_idx, text
            FROM {FTS_TABLE}
            WHERE book_id = ? AND text LIKE ? ESCAPE '\\'
            ORDER BY chapter_idx ASC, para_idx ASC
            LIMIT ?""",
        (book_id, pattern, limit),
    ).fetchall()
    hits = []
    for r in rows:
        text = str(r[2])
        hits.append(_hit(r[0], r[1], text, query, -float(text.count(query))))
    return hits


def _search_like_chapters(conn: sqlite3.Connection, book_id: str, query: str, limit: int) -> list[dict]:
    """1 字查询：chapters 表 LIKE 兜底（degraded=true）。"""
    rows = conn.execute(
        "SELECT idx, paras_json FROM chapters WHERE book_id=? ORDER BY idx", (book_id,)
    ).fetchall()
    hits: list[dict] = []
    for row in rows:
        try:
            paras = json.loads(row[1])
        except (TypeError, ValueError):
            continue
        if not isinstance(paras, list):
            continue
        for para_idx, para in enumerate(paras):
            text = str(para)
            if query in text:
                hits.append(_hit(row[0], para_idx, text, query, -float(text.count(query))))
                if len(hits) >= limit:
                    return hits
    return hits


def search(conn: sqlite3.Connection, book_id: str, query: str, limit: int = DEFAULT_LIMIT) -> dict:
    """检索入口（先按需补索引，再按查询长度选路径）。q 为空请由 API 层报 400。"""
    query = "" if query is None else str(query).strip()
    if not query:
        raise ValueError("empty_query")
    limit = _clamp_limit(limit)

    state = ensure_book_index(conn, book_id)
    use_fts = has_fts(conn) and state["degraded"] == 0
    degraded = bool(state["degraded"])

    hits: list[dict] = []
    if use_fts and len(query) >= MIN_MATCH_CHARS:
        try:
            hits = _search_match(conn, book_id, query, limit)
        except sqlite3.OperationalError:
            # MATCH 语法/索引异常 -> 降级 LIKE，并持久化 degraded
            _set_degraded(conn, book_id, 1)
            degraded = True
            use_fts = False

    if not hits:
        if len(query) == 1:
            # 单字：契约要求 chapters LIKE 兜底 + degraded=true
            degraded = True
            hits = _search_like_chapters(conn, book_id, query, limit)
        elif len(query) == MIN_MATCH_CHARS - 1 and use_fts:
            # 2 字：trigram MATCH 不可用，扫 para_fts 的 text 列 LIKE（真实命中）
            hits = _search_like_fts(conn, book_id, query, limit)
        elif not use_fts:
            # FTS 不可用：任何长度都退化为 chapters LIKE
            degraded = True
            hits = _search_like_chapters(conn, book_id, query, limit)

    current = get_state(conn, book_id)
    return {
        "hits": hits,
        "index": {
            "chapters_indexed": int(current["chapters_indexed"]),
            "degraded": bool(degraded),
        },
        "degraded": bool(degraded),
    }
