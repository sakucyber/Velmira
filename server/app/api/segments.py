"""分段 API：契约 4.10（POST /api/books/{id}/segment）与 4.11（GET .../segments）。

本模块只做 HTTP + DB 读写；分段/打分全在 server/app/pipeline/（纯函数，可单测）。
错误响应统一 {"error": {"code","message"}}，不用 FastAPI 默认的 detail。
"""
from __future__ import annotations

import json
import time

from fastapi import APIRouter, Body, Query
from fastapi.responses import JSONResponse

from .. import db
from ..pipeline.segmenter import (
    STRATEGY_PARAGRAPH_WINDOW,
    SUPPORTED_STRATEGIES,
    build_segments,
    compute_params_hash,
    normalize_params,
)

router = APIRouter()

_DEFAULT_LIMIT = 50
_MAX_LIMIT = 1000


def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code, content={"error": {"code": code, "message": message}})


def _book_exists(conn, book_id: str) -> bool:
    return conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone() is not None


def _safe_json(text):
    try:
        value = json.loads(text)
        return value if isinstance(value, dict) else {}
    except (TypeError, ValueError):
        return {}


def _load_chapters(conn, book_id: str) -> list[dict]:
    """从 chapters 表读段落（A 的 importer 已完成清洗，C 不再解析原文）。"""
    rows = conn.execute(
        "SELECT idx, title, paras_json FROM chapters WHERE book_id=? ORDER BY idx", (book_id,)
    ).fetchall()
    chapters = []
    for row in rows:
        try:
            paras = json.loads(row["paras_json"])
        except (TypeError, ValueError):
            paras = []
        if not isinstance(paras, list):
            paras = []
        chapters.append({
            "chapter": int(row["idx"]),
            "title": row["title"],
            "paragraphs": [str(p) for p in paras if p is not None and str(p).strip() != ""],
        })
    return chapters


def _cached_stats(conn, book_id: str, params_hash: str) -> dict:
    rows = conn.execute(
        "SELECT features_json, selected FROM segments WHERE book_id=? AND params_hash=?",
        (book_id, params_hash),
    ).fetchall()
    picked_chars = 0
    selected = 0
    for row in rows:
        chars = int(_safe_json(row["features_json"]).get("chars", 0) or 0)
        if int(row["selected"] or 0) == 1:
            selected += 1
            picked_chars += chars
    return {"segments": len(rows), "selected": selected, "picked_chars": picked_chars}


def _replace_segments(conn, book_id: str, params_hash: str, strategy: str, segments: list[dict]) -> None:
    """覆盖写：M0 每本书只保留一套分段结果。

    原因：契约 4.11 的 GET 没有 params_hash 过滤参数，多套结果并存会让前端拿到混排数据；
    且 segments.id 是全局主键（契约 DDL），同一 c33-s0 无法跨 params_hash 共存。
    后续要多套并存需要契约升级（GET 增加 params_hash 过滤 + id 编码 hash）。
    """
    with conn:
        conn.execute("DELETE FROM segments WHERE book_id=?", (book_id,))
        if not segments:
            return
        conn.executemany(
            """INSERT OR REPLACE INTO segments
               (id, book_id, chapter, idx, strategy, params_hash, start_para, end_para,
                text, features_json, score, raw, selected)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)""",
            [
                (
                    seg["id"], book_id, int(seg["chapter"]), int(seg["idx"]), strategy, params_hash,
                    int(seg["start_para"]), int(seg["end_para"]), seg["text"],
                    json.dumps(seg["features"], ensure_ascii=False),
                    float(seg["score"]), float(seg["raw"]), int(seg["selected"]),
                )
                for seg in segments
            ],
        )


@router.post("/api/books/{book_id}/segment")
def segment_book(book_id: str, payload: dict | None = Body(default=None)):
    started = time.perf_counter()
    payload = payload if isinstance(payload, dict) else {}

    strategy = str(payload.get("strategy") or STRATEGY_PARAGRAPH_WINDOW)
    if strategy not in SUPPORTED_STRATEGIES:
        return _error(400, "unsupported_strategy", f"不支持的策略: {strategy}（M0 仅支持 {STRATEGY_PARAGRAPH_WINDOW}）")

    raw_params = payload.get("params") or {}
    if not isinstance(raw_params, dict):
        return _error(400, "invalid_params", "params 必须是 JSON 对象")
    try:
        params = normalize_params(raw_params)
        per_chapter = int(payload.get("per_chapter", 1))
    except (TypeError, ValueError) as exc:
        return _error(400, "invalid_params", str(exc))
    if per_chapter < 0 or per_chapter > 50:
        return _error(400, "invalid_params", "per_chapter 必须在 0..50")

    force = bool(payload.get("force", False))
    params_hash = compute_params_hash(strategy, params, per_chapter)

    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")

        # 契约 4.10：同 (book_id, params_hash) 已存在且 force=false -> 直接返回缓存统计
        if not force:
            stats = _cached_stats(conn, book_id, params_hash)
            if stats["segments"] > 0:
                return {
                    "book_id": book_id,
                    "strategy": strategy,
                    "params_hash": params_hash,
                    "segments": stats["segments"],
                    "selected": stats["selected"],
                    "picked_chars": stats["picked_chars"],
                    "elapsed_ms": int((time.perf_counter() - started) * 1000),
                    "cached": True,
                }

        chapters = _load_chapters(conn, book_id)
        result = build_segments(chapters, params, per_chapter=per_chapter, strategy=strategy)
        _replace_segments(conn, book_id, params_hash, strategy, result["segments"])
        stats = result["stats"]
        return {
            "book_id": book_id,
            "strategy": strategy,
            "params_hash": params_hash,
            "segments": int(stats["segments"]),
            "selected": int(stats["selected"]),
            "picked_chars": int(stats["picked_chars"]),
            "elapsed_ms": int((time.perf_counter() - started) * 1000),
            "cached": False,
        }
    finally:
        conn.close()


@router.get("/api/books/{book_id}/segments")
def list_segments(
    book_id: str,
    chapter: int | None = Query(default=None),
    selected: int | None = Query(default=None),
    limit: int = Query(default=_DEFAULT_LIMIT),
):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        if selected is not None and selected not in (0, 1):
            return _error(400, "invalid_params", "selected 只能是 0 或 1")
        limit_value = max(1, min(_MAX_LIMIT, int(limit)))

        sql = (
            "SELECT id, chapter, idx, start_para, end_para, text, features_json, raw, score, selected "
            "FROM segments WHERE book_id=?"
        )
        args: list = [book_id]
        if chapter is not None:
            sql += " AND chapter=?"
            args.append(int(chapter))
        if selected is not None:
            sql += " AND selected=?"
            args.append(int(selected))
        sql += " ORDER BY chapter ASC, score DESC, idx ASC LIMIT ?"
        args.append(limit_value)
        rows = conn.execute(sql, args).fetchall()

        titles = {
            int(row[0]): row[1]
            for row in conn.execute("SELECT idx, title FROM chapters WHERE book_id=?", (book_id,)).fetchall()
        }

        segments = []
        for row in rows:
            features = _safe_json(row["features_json"])
            text = row["text"] or ""
            segments.append({
                "id": row["id"],
                "chapter": int(row["chapter"]),
                "idx": int(row["idx"]),
                "start_para": int(row["start_para"]),
                "end_para": int(row["end_para"]),
                "chars": int(features.get("chars", len(text)) or 0),
                "raw": float(row["raw"]),
                "score": float(row["score"]),
                "selected": int(row["selected"]),
                "title": titles.get(int(row["chapter"]), ""),
                "head": text.split("\n", 1)[0][:60],
            })
        return {"segments": segments}
    finally:
        conn.close()
