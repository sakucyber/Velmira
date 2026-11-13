"""M2 AI API：provider 配置、scene 抽取、人物卡查看/编辑/冻结。"""
from __future__ import annotations

import json
import time
from typing import Any

from fastapi import APIRouter, Body, Query
from fastapi.responses import JSONResponse

from .. import db
from ..ai.extraction import (
    _character_public,
    _verify_identity,
    extract_segments,
    identity_hash,
    render_identity,
)
from ..ai.models import CARD_FIELDS, merge_card, stable_json
from ..ai.providers import make_provider
from ..ai.prompts import PROMPT_VERSION

router = APIRouter()


def _error(status_code: int, code: str, message: str) -> JSONResponse:
    return JSONResponse(status_code=status_code, content={"error": {"code": code, "message": message}})


def _json(value: Any, default: Any):
    try:
        result = json.loads(value)
        return result if isinstance(result, type(default)) else default
    except (TypeError, ValueError):
        return default


def _book_exists(conn, book_id: str) -> bool:
    return conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone() is not None


def _config_row(conn, book_id: str):
    row = conn.execute("SELECT * FROM ai_provider_configs WHERE book_id=?", (book_id,)).fetchone()
    if row is not None:
        return row
    now = int(time.time())
    with conn:
        conn.execute(
            """INSERT OR IGNORE INTO ai_provider_configs
               (book_id,provider,base_url,model,api_key_env,temperature,prompt_version,enabled,created_at,updated_at)
               VALUES(?,?,?,?,?,?,?,?,?,?)""",
            (book_id, "mock", None, None, None, 0.2, PROMPT_VERSION, 1, now, now),
        )
    return conn.execute("SELECT * FROM ai_provider_configs WHERE book_id=?", (book_id,)).fetchone()


def _config_public(row) -> dict[str, Any]:
    return {
        "book_id": row["book_id"],
        "provider": row["provider"],
        "base_url": row["base_url"],
        "model": row["model"],
        "api_key_env": row["api_key_env"],
        "temperature": float(row["temperature"]),
        "prompt_version": int(row["prompt_version"]),
        "enabled": bool(row["enabled"]),
        "updated_at": int(row["updated_at"]),
    }


def _config_dict(row) -> dict[str, Any]:
    data = _config_public(row)
    return data


@router.get("/api/books/{book_id}/ai/config")
def get_ai_config(book_id: str):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        return {"config": _config_public(_config_row(conn, book_id))}
    finally:
        conn.close()


@router.patch("/api/books/{book_id}/ai/config")
def patch_ai_config(book_id: str, payload: dict | None = Body(default=None)):
    payload = payload if isinstance(payload, dict) else {}
    allowed = {"provider", "base_url", "model", "api_key_env", "temperature", "prompt_version", "enabled"}
    unknown = sorted(set(payload) - allowed)
    if unknown:
        return _error(400, "invalid_ai_config", f"不支持的配置字段: {', '.join(unknown)}")
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        old = _config_row(conn, book_id)
        values = _config_dict(old)
        values.update(payload)
        provider = str(values.get("provider") or "mock").strip().lower()
        if provider not in {"mock", "offline", "openai", "openai-compatible", "openai_compat", "ollama", "llamacpp"}:
            return _error(400, "invalid_ai_config", f"不支持的文本 provider: {provider}")
        try:
            temperature = float(values.get("temperature", 0.2))
            if not 0 <= temperature <= 2:
                raise ValueError
            prompt_version = int(values.get("prompt_version", PROMPT_VERSION))
            if prompt_version < 1:
                raise ValueError
            enabled = 1 if bool(values.get("enabled", True)) else 0
        except (TypeError, ValueError):
            return _error(400, "invalid_ai_config", "temperature 必须在 0..2，prompt_version 必须为正整数")
        now = int(time.time())
        with conn:
            conn.execute(
                """UPDATE ai_provider_configs SET provider=?,base_url=?,model=?,api_key_env=?,temperature=?,prompt_version=?,enabled=?,updated_at=? WHERE book_id=?""",
                (provider, str(values.get("base_url") or "") or None, str(values.get("model") or "") or None,
                 str(values.get("api_key_env") or "") or None, temperature, prompt_version, enabled, now, book_id),
            )
        return {"config": _config_public(conn.execute("SELECT * FROM ai_provider_configs WHERE book_id=?", (book_id,)).fetchone())}
    finally:
        conn.close()


@router.post("/api/books/{book_id}/ai/extract")
async def extract_ai(book_id: str, payload: dict | None = Body(default=None)):
    payload = payload if isinstance(payload, dict) else {}
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        config_row = _config_row(conn, book_id)
        config = _config_public(config_row)
        from ..ai.settings import load_settings
        shared = load_settings()
        if config["provider"] in {"mock", "offline"} and shared["enabled"]:
            config.update(provider="openai-compatible", base_url=shared["base_url"], model=shared["model"], api_key_env="", local_settings=True)
        requested_provider = payload.get("provider")
        provider_name = str(requested_provider or config["provider"] or "mock").lower()
        if not config["enabled"] and not requested_provider:
            return _error(409, "ai_disabled", "文本 provider 已禁用")
        try:
            provider = make_provider(config, requested_provider)
        except ValueError as exc:
            return _error(400, "invalid_ai_provider", str(exc))
        try:
            prompt_version = int(payload.get("prompt_version", config["prompt_version"] or PROMPT_VERSION))
            if prompt_version < 1:
                raise ValueError
        except (TypeError, ValueError):
            return _error(400, "invalid_ai_request", "prompt_version 必须是正整数")
        force = bool(payload.get("force", False))
        requested_ids = payload.get("segment_ids")
        if requested_ids is not None and (not isinstance(requested_ids, list) or not all(isinstance(item, str) for item in requested_ids)):
            return _error(400, "invalid_ai_request", "segment_ids 必须是字符串数组")
        sql = "SELECT id,chapter,idx,text,score,selected FROM segments WHERE book_id=?"
        args: list[Any] = [book_id]
        if requested_ids is not None:
            if not requested_ids:
                return _error(400, "no_selected_segments", "指定范围内没有可抽取的片段，请先运行分段")
            placeholders = ",".join("?" for _ in requested_ids)
            sql += f" AND id IN ({placeholders})"
            args.extend(requested_ids)
        else:
            sql += " AND selected=1"
        sql += " ORDER BY chapter,idx"
        rows = conn.execute(sql, args).fetchall()
        if requested_ids and len(rows) != len(set(requested_ids)):
            return _error(404, "segment_not_found", "有片段不属于该书或不存在")
        if not rows:
            return _error(400, "no_selected_segments", "没有可抽取的 selected 片段，请先运行分段并选片")
        segments = [dict(row) for row in rows]
        snapshot = {"provider": provider_name, "model": config.get("model"), "base_url": config.get("base_url"),
                    "api_key_env": config.get("api_key_env"), "temperature": config.get("temperature"),
                    "prompt_version": prompt_version}
        result = await extract_segments(conn=conn, book_id=book_id, segments=segments, provider=provider,
                                        provider_name=provider_name, config_snapshot=snapshot,
                                        prompt_version=prompt_version, force=force)
        return {"book_id": book_id, "prompt_version": prompt_version, **result}
    finally:
        conn.close()


@router.post("/api/books/{book_id}/characters/extract")
async def extract_characters_compat(book_id: str, payload: dict | None = Body(default=None)):
    """附录路径兼容入口；M2 的 scene 与人物卡在同一抽取运行中生成。"""
    payload = dict(payload) if isinstance(payload, dict) else {}
    scope = str(payload.get("scope") or "selected").lower()
    if scope in {"first_n", "first3", "first_3"} and "segment_ids" not in payload:
        n = payload.get("chapters", payload.get("first_n", 3))
        try:
            n = max(1, min(3, int(n)))
        except (TypeError, ValueError):
            n = 3
        conn = db.get_conn()
        try:
            ids = [row["id"] for row in conn.execute("SELECT id FROM segments WHERE book_id=? AND chapter<? ORDER BY chapter,idx", (book_id, n)).fetchall()]
        finally:
            conn.close()
        payload["segment_ids"] = ids
    elif scope == "full" and "segment_ids" not in payload:
        conn = db.get_conn()
        try:
            payload["segment_ids"] = [row["id"] for row in conn.execute("SELECT id FROM segments WHERE book_id=? ORDER BY chapter,idx", (book_id,)).fetchall()]
        finally:
            conn.close()
    return await extract_ai(book_id, payload)


@router.get("/api/books/{book_id}/ai/runs")
def list_ai_runs(book_id: str, limit: int = Query(default=20, ge=1, le=100)):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        rows = conn.execute("SELECT * FROM ai_runs WHERE book_id=? ORDER BY created_at DESC LIMIT ?", (book_id, limit)).fetchall()
        return {"runs": [{"id": row["id"], "book_id": row["book_id"], "kind": row["kind"], "provider": row["provider"],
                           "config": _json(row["config_json"], {}), "status": row["status"], "stats": _json(row["stats_json"], {}),
                           "error": _json(row["error"], []) if row["error"] and row["error"].startswith("[") else row["error"],
                           "created_at": row["created_at"], "updated_at": row["updated_at"]} for row in rows]}
    finally:
        conn.close()


@router.get("/api/books/{book_id}/ai/scenes")
def list_ai_scenes(book_id: str, status: str | None = Query(default=None), limit: int = Query(default=100, ge=1, le=1000), offset: int = Query(default=0, ge=0)):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        sql = "SELECT * FROM scenes WHERE book_id=?"
        args: list[Any] = [book_id]
        if status:
            sql += " AND status=?"
            args.append(status)
        sql += " ORDER BY chapter_id, segment_id, id LIMIT ? OFFSET ?"
        args.extend([limit + 1, offset])
        rows = conn.execute(sql, args).fetchall()
        return {"has_more": len(rows) > limit, "scenes": [{"id": row["id"], "book_id": row["book_id"], "chapter_id": row["chapter_id"],
                             "segment_id": row["segment_id"], "plan": _json(row["plan_json"], {}), "status": row["status"],
                             "prompt_version": row["prompt_version"], "created_at": row["created_at"], "updated_at": row["updated_at"]} for row in rows[:limit]]}
    finally:
        conn.close()


def _list_characters(conn, book_id: str):
    rows = conn.execute("SELECT * FROM characters WHERE book_id=? ORDER BY id", (book_id,)).fetchall()
    return [_character_public(row) for row in rows]


@router.get("/api/books/{book_id}/ai/characters")
def list_ai_characters(book_id: str):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        return {"characters": _list_characters(conn, book_id)}
    finally:
        conn.close()


def _get_character(conn, book_id: str, character_id: str):
    return conn.execute("SELECT * FROM characters WHERE id=? AND book_id=?", (character_id, book_id)).fetchone()


@router.get("/api/books/{book_id}/ai/characters/{character_id}")
def get_ai_character(book_id: str, character_id: str):
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        row = _get_character(conn, book_id, character_id)
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        return {"character": _character_public(row)}
    finally:
        conn.close()


@router.patch("/api/books/{book_id}/ai/characters/{character_id}")
def patch_ai_character(book_id: str, character_id: str, payload: dict | None = Body(default=None)):
    payload = payload if isinstance(payload, dict) else {}
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        row = _get_character(conn, book_id, character_id)
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        old_card = _json(row["card_json"], {})
        incoming_card = payload.get("card", payload)
        if not isinstance(incoming_card, dict):
            return _error(400, "invalid_character", "card 必须是 JSON 对象")
        unknown = set(incoming_card) - set(CARD_FIELDS) - {"evidence", "variants"}
        if unknown:
            return _error(400, "invalid_character", f"不支持的人物卡字段: {', '.join(sorted(unknown))}")
        if row["frozen"] and any(field in incoming_card and incoming_card[field] != old_card.get(field) for field in CARD_FIELDS):
            return _error(409, "character_frozen", "人物卡 identity 已冻结，请先显式解冻并升版本")
        merged = merge_card(old_card, incoming_card, frozen=bool(row["frozen"]))
        # Manual edits must replace known values; automatic extraction remains fill-only.
        if not row["frozen"]:
            for field in CARD_FIELDS:
                if field in incoming_card:
                    merged[field] = incoming_card[field]
        aliases = payload.get("aliases", _json(row["aliases_json"], []))
        if not isinstance(aliases, list):
            return _error(400, "invalid_character", "aliases 必须是数组")
        aliases = [str(item).strip() for item in aliases if str(item).strip()]
        name = str(payload.get("name", row["name"])).strip() or row["name"]
        now = int(time.time())
        with conn:
            conn.execute("UPDATE characters SET name=?,aliases_json=?,card_json=?,updated_at=? WHERE id=? AND book_id=?",
                         (name, stable_json(aliases), stable_json(merged), now, character_id, book_id))
        return {"character": _character_public(_get_character(conn, book_id, character_id))}
    finally:
        conn.close()


@router.post("/api/books/{book_id}/ai/characters/{character_id}/freeze")
def freeze_ai_character(book_id: str, character_id: str, payload: dict | None = Body(default=None)):
    payload = payload if isinstance(payload, dict) else {}
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        row = _get_character(conn, book_id, character_id)
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        if row["frozen"] and not _verify_identity(row):
            return _error(409, "identity_hash_mismatch", "冻结人物卡的 identity_hash 校验失败，请解冻后重新冻结")
        card = _json(row["card_json"], {})
        block = render_identity(card, row["name"], payload.get("chapter"))
        digest = identity_hash(block)
        now = int(time.time())
        with conn:
            conn.execute("UPDATE characters SET frozen=1,identity_block=?,identity_hash=?,updated_at=? WHERE id=? AND book_id=?",
                         (block, digest, now, character_id, book_id))
        return {"character": _character_public(_get_character(conn, book_id, character_id))}
    finally:
        conn.close()


@router.post("/api/books/{book_id}/ai/characters/{character_id}/unfreeze")
def unfreeze_ai_character(book_id: str, character_id: str, payload: dict | None = Body(default=None)):
    payload = payload if isinstance(payload, dict) else {}
    if payload.get("confirm") is not True:
        return _error(400, "confirmation_required", "解冻会升版本并保留旧身份快照，需要 confirm=true")
    conn = db.get_conn()
    try:
        if not _book_exists(conn, book_id):
            return _error(404, "book_not_found", f"书籍不存在: {book_id}")
        row = _get_character(conn, book_id, character_id)
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        card = _json(row["card_json"], {})
        history = card.setdefault("identity_history", [])
        if row["identity_block"]:
            snapshot = {"version": int(row["version"]), "identity_block": row["identity_block"], "identity_hash": row["identity_hash"]}
            if snapshot not in history:
                history.append(snapshot)
        now = int(time.time())
        with conn:
            conn.execute("UPDATE characters SET frozen=0,identity_block=NULL,identity_hash=NULL,version=?,card_json=?,updated_at=? WHERE id=? AND book_id=?",
                         (int(row["version"] or 1) + 1, stable_json(card), now, character_id, book_id))
        return {"character": _character_public(_get_character(conn, book_id, character_id))}
    finally:
        conn.close()


@router.post("/api/characters/{character_id}/freeze")
def freeze_character_compat(character_id: str, payload: dict | None = Body(default=None)):
    conn = db.get_conn()
    try:
        row = conn.execute("SELECT book_id FROM characters WHERE id=?", (character_id,)).fetchone()
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        return freeze_ai_character(row["book_id"], character_id, payload)
    finally:
        conn.close()


@router.post("/api/characters/{character_id}/unfreeze")
def unfreeze_character_compat(character_id: str, payload: dict | None = Body(default=None)):
    conn = db.get_conn()
    try:
        row = conn.execute("SELECT book_id FROM characters WHERE id=?", (character_id,)).fetchone()
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        return unfreeze_ai_character(row["book_id"], character_id, payload)
    finally:
        conn.close()


# 交接包附录还列过全局 /api/characters/{id}；提供只读/编辑兼容入口。
@router.get("/api/characters/{character_id}")
def get_character_compat(character_id: str):
    conn = db.get_conn()
    try:
        row = conn.execute("SELECT * FROM characters WHERE id=?", (character_id,)).fetchone()
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        return {"character": _character_public(row)}
    finally:
        conn.close()


@router.patch("/api/characters/{character_id}")
def patch_character_compat(character_id: str, payload: dict | None = Body(default=None)):
    conn = db.get_conn()
    try:
        row = conn.execute("SELECT book_id FROM characters WHERE id=?", (character_id,)).fetchone()
        if row is None:
            return _error(404, "character_not_found", f"人物不存在: {character_id}")
        # 复用嵌套路由的实现逻辑，保持全局契约路径可用。
        return patch_ai_character(row["book_id"], character_id, payload)
    finally:
        conn.close()
