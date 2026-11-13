"""M2 抽取编排：候选片段 → provider → 校验 → scene/人物卡幂等落库。"""
from __future__ import annotations

import asyncio
import hashlib
import json
import time
from contextlib import nullcontext
from typing import Any

from .. import db
from .models import (
    AIModelError,
    identity_hash,
    merge_card,
    normalize_character,
    normalize_name,
    normalize_scene,
    render_identity,
    stable_json,
)
from .prompts import PROMPT_VERSION, SCENE_SYSTEM, build_scene_user
from .providers import TextProvider
from . import retry


def input_hash(segment_text: str, prompt_version: int, provider_config=None) -> str:
    fingerprint = stable_json(provider_config) if provider_config else ""
    return hashlib.sha256(f"{segment_text}\n\0prompt:{int(prompt_version)}\n{fingerprint}".encode("utf-8")).hexdigest()


def character_id(book_id: str, name: str) -> str:
    return "ch_" + hashlib.sha256((book_id + "\0" + normalize_name(name)).encode("utf-8")).hexdigest()[:10]


def scene_id(book_id: str, segment_id: str, prompt_version: int) -> str:
    return "sc_" + hashlib.sha256(f"{book_id}\0{segment_id}\0{prompt_version}".encode("utf-8")).hexdigest()[:12]


def _json_load(value: Any, default: Any):
    try:
        parsed = json.loads(value)
        return parsed if isinstance(parsed, type(default)) else default
    except (TypeError, ValueError):
        return default


def _same_character(row, name: str, aliases: list[str]) -> bool:
    keys = {normalize_name(name), *(normalize_name(alias) for alias in aliases)}
    existing = {normalize_name(row["name"]), *(_json_load(row["aliases_json"], []))}
    existing = {normalize_name(item) for item in existing if item}
    return bool(keys & existing)


def _verify_identity(row) -> bool:
    block = row["identity_block"] or ""
    digest = row["identity_hash"] or ""
    return bool(block and digest and identity_hash(block) == digest)


def _character_public(row) -> dict[str, Any]:
    card = _json_load(row["card_json"], {})
    integrity_ok = True
    if row["frozen"]:
        integrity_ok = _verify_identity(row)
    return {
        "id": row["id"],
        "book_id": row["book_id"],
        "name": row["name"],
        "aliases": _json_load(row["aliases_json"], []),
        "card": card,
        "frozen": bool(row["frozen"]),
        "identity_block": row["identity_block"],
        "identity_hash": row["identity_hash"],
        "identity_integrity_ok": integrity_ok,
        "version": int(row["version"] or 1),
        "first_chapter": int(row["first_chapter"] or 0),
        "created_at": int(row["created_at"] or 0),
        "updated_at": int(row["updated_at"] or 0),
    }


def _merge_character(conn, book_id: str, candidate: dict[str, Any], chapter: int, source_text: str, *, commit=True) -> str:
    candidate = normalize_character(candidate, source_text, chapter)
    rows = conn.execute("SELECT * FROM characters WHERE book_id=? ORDER BY id", (book_id,)).fetchall()
    row = next((item for item in rows if _same_character(item, candidate["name"], candidate["aliases"])), None)
    now = int(time.time())
    if row is None:
        cid = character_id(book_id, candidate["name"])
        card = dict(candidate["card"])
        card["evidence"] = list(candidate["evidence"])
        aliases = [alias for alias in candidate["aliases"] if normalize_name(alias) != normalize_name(candidate["name"])]
        with conn if commit else nullcontext():
            conn.execute(
                """INSERT OR IGNORE INTO characters
                   (id,book_id,name,aliases_json,card_json,frozen,identity_block,identity_hash,version,first_chapter,created_at,updated_at)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
                (cid, book_id, candidate["name"], stable_json(aliases), stable_json(card), 0, None, None, 1, chapter, now, now),
            )
        return cid
    cid = row["id"]
    old_card = _json_load(row["card_json"], {})
    merged = merge_card(old_card, {**candidate["card"], "evidence": candidate["evidence"]}, frozen=bool(row["frozen"]))
    aliases = _json_load(row["aliases_json"], [])
    for alias in [candidate["name"], *candidate["aliases"]]:
        if normalize_name(alias) != normalize_name(row["name"]) and alias not in aliases:
            aliases.append(alias)
    # Frozen identity is immutable. A corrupted hash is surfaced to callers and cannot be silently fixed.
    with conn if commit else nullcontext():
        conn.execute(
            "UPDATE characters SET aliases_json=?, card_json=?, first_chapter=MIN(first_chapter,?), updated_at=? WHERE id=?",
            (stable_json(aliases), stable_json(merged), chapter, now, cid),
        )
    return cid


async def call_provider(provider: TextProvider, *, system: str, user: str, schema: dict | None = None, temperature: float = 0.2) -> Any:
    return await provider.complete(system=system, user=user, schema=schema, temperature=temperature)


async def extract_one(provider: TextProvider, *, segment: dict[str, Any], prompt_version: int, temperature: float = 0.2) -> dict[str, Any]:
    """Each segment has one three-attempt budget, including JSON validation."""
    source_text = str(segment.get("text") or "")
    chapter = int(segment.get("chapter") or 0)
    def validate(raw):
        if isinstance(raw, str): raw = json.loads(raw)
        return normalize_scene(raw, source_text, chapter)
    try:
        return await retry.complete(provider, system=SCENE_SYSTEM, user=build_scene_user(source_text, chapter),
                                    temperature=temperature, validate=validate)
    except Exception:
        raise AIModelError("文本 AI 已尝试 3 次仍失败，请检查连接与结构化输出") from None


async def extract_segments(
    *,
    conn,
    book_id: str,
    segments: list[dict[str, Any]],
    provider: TextProvider,
    provider_name: str,
    config_snapshot: dict[str, Any],
    prompt_version: int = PROMPT_VERSION,
    force: bool = False,
) -> dict[str, Any]:
    """处理一批已筛选片段，单片段失败写 needs_review 后继续。"""
    run_id = "run_" + hashlib.sha256(f"{book_id}\0{time.time_ns()}".encode()).hexdigest()[:14]
    now = int(time.time())
    with conn:
        conn.execute(
            "INSERT INTO ai_runs(id,book_id,kind,provider,config_json,status,stats_json,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
            (run_id, book_id, "scene_extract", provider_name, stable_json(config_snapshot), "running", "{}", now, now),
        )
    stats = {"requested": len(segments), "processed": 0, "cached": 0, "needs_review": 0, "scenes": 0, "characters": 0}
    errors: list[dict[str, str]] = []
    for segment in segments:
        segment_id = str(segment["id"])
        source_text = str(segment.get("text") or "")
        digest = input_hash(source_text, prompt_version, config_snapshot)
        cached = conn.execute(
            "SELECT result_json,status,error FROM ai_extractions WHERE book_id=? AND segment_id=? AND prompt_version=? AND input_hash=?",
            (book_id, segment_id, prompt_version, digest),
        ).fetchone()
        if cached is not None and cached["status"] == "done" and not force:
            result = _json_load(cached["result_json"], {})
            stats["cached"] += 1
        else:
            try:
                result = await extract_one(provider, segment=segment, prompt_version=prompt_version, temperature=float(config_snapshot.get("temperature", 0.2)))
            except Exception as exc:
                message = str(exc)[:500]
                stats["needs_review"] += 1
                errors.append({"segment_id": segment_id, "error": message})
                with conn:
                    conn.execute(
                        """INSERT INTO ai_extractions(book_id,segment_id,prompt_version,input_hash,result_json,status,error,created_at,updated_at)
                           VALUES(?,?,?,?,?,?,?,?,?)
                           ON CONFLICT(book_id,segment_id,prompt_version,input_hash) DO UPDATE SET result_json=excluded.result_json,status=excluded.status,error=excluded.error,updated_at=excluded.updated_at""",
                        (book_id, segment_id, prompt_version, digest, "{}", "needs_review", message, now, int(time.time())),
                    )
                continue
            with conn:
                conn.execute(
                    """INSERT INTO ai_extractions(book_id,segment_id,prompt_version,input_hash,result_json,status,error,created_at,updated_at)
                       VALUES(?,?,?,?,?,?,?,?,?)
                       ON CONFLICT(book_id,segment_id,prompt_version,input_hash) DO UPDATE SET result_json=excluded.result_json,status=excluded.status,error=NULL,updated_at=excluded.updated_at""",
                    (book_id, segment_id, prompt_version, digest, stable_json(result), "done", None, now, int(time.time())),
                )
        if not isinstance(result, dict):
            stats["needs_review"] += 1
            continue
        chapter = int(segment.get("chapter") or 0)
        scene_characters = result.get("characters") if isinstance(result.get("characters"), list) else []
        ids = []
        for candidate in scene_characters:
            try:
                ids.append(_merge_character(conn, book_id, candidate, chapter, source_text))
            except Exception as exc:
                errors.append({"segment_id": segment_id, "error": f"character: {str(exc)[:400]}"})
        scene_plan = dict(result)
        scene_plan["character_ids"] = sorted(set(ids))
        sid = scene_id(book_id, segment_id, prompt_version)
        with conn:
            conn.execute(
                """INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,status,prompt_version,created_at,updated_at)
                   VALUES(?,?,?,?,?,?,?,?,?)
                   ON CONFLICT(book_id,segment_id,prompt_version) DO UPDATE SET plan_json=excluded.plan_json,status=excluded.status,updated_at=excluded.updated_at""",
                (sid, book_id, chapter, segment_id, stable_json(scene_plan), "draft", prompt_version, now, int(time.time())),
            )
        stats["processed"] += 1
        stats["scenes"] += 1
        stats["characters"] += len(set(ids))
    stats["errors"] = len(errors)
    with conn:
        conn.execute(
            "UPDATE ai_runs SET status=?,stats_json=?,error=?,updated_at=? WHERE id=?",
            ("done" if not errors or stats["processed"] else "needs_review", stable_json(stats), stable_json(errors) if errors else None, int(time.time()), run_id),
        )
    return {"run_id": run_id, "status": "done" if not errors else "needs_review", **stats}


def run_async(coro):
    """供同步测试/脚本使用；HTTP async 路由直接 await。"""
    return asyncio.run(coro)


__all__ = [
    "PROMPT_VERSION",
    "character_id",
    "scene_id",
    "input_hash",
    "extract_one",
    "extract_segments",
    "render_identity",
    "identity_hash",
    "_character_public",
    "_verify_identity",
]
