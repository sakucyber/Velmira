from __future__ import annotations

import asyncio
import json
import time

from fastapi import APIRouter, Body

from ..ai import director, settings
from ..ai.models import stable_json
from ..ai.providers import request_json, make_provider
from ..manga import service as s
from ..manga.models import MangaError, digest
from .manga import connection, fields

router = APIRouter()


@router.get("/api/ai/settings")
def get_settings():
    return {"settings": settings.public_settings()}


@router.patch("/api/ai/settings")
def patch_settings(payload: dict = Body(...)):
    try:
        return {"settings": settings.save_settings(payload)}
    except ValueError as exc:
        raise MangaError(str(exc)) from None


@router.get("/api/ai/models")
async def list_models():
    cfg = settings.load_settings()
    if not cfg["base_url"]:
        raise MangaError("请先保存 API 地址和密钥")
    try:
        data = await asyncio.to_thread(request_json, cfg["base_url"] + "/models", api_key=settings.api_key(cfg))
        models = sorted({item["id"] for item in data.get("data", []) if isinstance(item, dict) and isinstance(item.get("id"), str)})
        if not models:
            raise ValueError()
        return {"models": models[:1000]}
    except Exception:
        raise MangaError("无法获取模型列表，请检查 API 地址与密钥，也可以手动填写模型名称", "text_models_failed", 502) from None


@router.post("/api/ai/probe")
async def probe():
    cfg = director.text_config()
    try:
        raw = await make_provider(cfg).complete(system="Return only JSON.", user='Return {"ok":true}.', schema={"type": "object"})
        if director.json_object(raw).get("ok") is not True:
            raise ValueError()
        return {"ok": True, "model": cfg["model"]}
    except Exception:
        raise MangaError("文本 AI 检测失败，请检查模型、地址和密钥", "text_probe_failed", 502) from None


@router.post("/api/books/{book_id}/manga/plan")
async def create_plan(book_id: str, payload: dict = Body(...)):
    fields(payload, {"scene_id", "form_id", "shot", "action", "para", "flashback"})
    cfg = director.text_config()
    with connection(book_id) as conn:
        material = director.prepare_material(conn, book_id, payload)
        source_hash = digest(material)
        cache_key = digest({"source": source_hash, "model": cfg["model"], "base_url": cfg["base_url"], "version": director.PLAN_VERSION})
        cached = conn.execute("SELECT plan_json FROM manga_text_plans WHERE book_id=? AND cache_key=?", (book_id, cache_key)).fetchone()
        if cached:
            return {"plan": json.loads(cached[0]), "cached": True}
    # Release SQLite's writer lock before both network requests.
    result = await director.write_plan(material, cfg)
    with connection(book_id) as conn:
        if digest(director.prepare_material(conn, book_id, payload)) != source_hash:
            raise MangaError("分析期间场景或人物发生变化，请重新分析", "text_plan_stale", 409)
        plan = {"id": s.uid("plan_"), "version": director.PLAN_VERSION, "source_hash": source_hash,
                "scene_id": material["scene_id"], "form_id": material["form_id"],
                "scope": {"chapter": material["chapter"], "current_chars": len(material["chapter_context"]),
                          "previous_chars": len(material["previous_chapter_tail"]), "target_chars": len(material["target_segment"])},
                "provider": {"base_url": cfg["base_url"], "model": cfg["model"]},
                "created_at": int(time.time()), **result}
        conn.execute("INSERT OR IGNORE INTO manga_text_plans VALUES(?,?,?,?,?)", (plan["id"], book_id, cache_key, stable_json(plan), plan["created_at"]))
        saved = conn.execute("SELECT plan_json FROM manga_text_plans WHERE book_id=? AND cache_key=?", (book_id, cache_key)).fetchone()
        return {"plan": json.loads(saved[0]), "cached": False}
