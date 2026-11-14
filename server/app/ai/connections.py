"""Local connection presets. Public metadata is separate from DPAPI credentials."""
import json
import re
import uuid
from .. import config
from . import settings


def entries():
    with settings._lock:
        path = config.DATA_DIR / "connections.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.is_file() else []


def public():
    result = []
    for item in entries():
        cfg = settings.public_settings("profile-" + item["id"])
        result.append({**{k: item[k] for k in ("id", "label", "kind", "models", "rpm", "max_concurrency")},
                       **{k: cfg[k] for k in ("base_url", "json_mode", "reference_api", "reasoning_effort", "key_configured")}})
    return result


def resolve(connection_id, model, kind):
    if not isinstance(connection_id, str) or not re.fullmatch(r"[a-z0-9-]{1,100}", connection_id):
        raise ValueError("连接或模型不存在")
    item = next((p for p in entries() if p["id"] == connection_id and p["kind"] == kind), None)
    if not item or not isinstance(model, str) or model not in item["models"]:
        raise ValueError("连接或模型不存在或类型不匹配")
    cfg = settings.public_settings("profile-" + connection_id)
    if not cfg["base_url"]:
        raise ValueError("连接尚未配置 API 地址")
    return {**cfg, "model": model, "credential_slot": "profile-" + connection_id,
            "connection_id": connection_id, "label": item["label"],
            "rpm": item["rpm"], "max_concurrency": item["max_concurrency"]}


def save_connection(payload):
    allowed = {"id", "label", "kind", "models", "base_url", "api_key", "rpm", "max_concurrency", "json_mode", "reference_api", "reasoning_effort"}
    if not isinstance(payload, dict) or set(payload) - allowed:
        raise ValueError("连接设置包含未知字段")
    kind, label, models = payload.get("kind"), payload.get("label"), payload.get("models")
    if kind not in ("text", "image"):
        raise ValueError("连接类型须为 text 或 image")
    if not isinstance(label, str) or not 1 <= len(label.strip()) <= 100:
        raise ValueError("连接名称须为 1–100 字符")
    if not isinstance(models, list) or not 0 <= len(models) <= 1000 or any(
            not isinstance(m, str) or not m.strip() or len(m) > 200 or any(ord(c) < 32 for c in m) for m in models):
        raise ValueError("请填写最多 1000 个有效模型名称")
    models = list(dict.fromkeys(m.strip() for m in models))
    connection_id = payload.get("id", "custom-" + uuid.uuid4().hex[:16])
    if not isinstance(connection_id, str) or not re.fullmatch(r"[a-z0-9-]{1,100}", connection_id):
        raise ValueError("连接标识无效")
    with settings._lock:
        items = entries()
        old = next((p for p in items if p["id"] == connection_id), None)
        if old and old["kind"] != kind:
            raise ValueError("不能改变已有连接的类型")
        # Updates cannot remove models still used by a pool.
        if old:
            from .routing import get_config
            if any(p["connection_id"] == connection_id and p["model"] not in models for p in get_config(kind)["models"]):
                from ..manga.models import MangaError
                raise MangaError("请先从模型池移除正在使用的模型", "model_in_use", 409)
        patch = {k: payload[k] for k in ("base_url", "api_key", "rpm", "max_concurrency", "json_mode", "reference_api", "reasoning_effort") if k in payload}
        patch.update(model=models[0] if models else "", enabled=bool(models))
        cfg = settings.save_settings(patch, "profile-" + connection_id)
        item = {"id": connection_id, "label": label.strip(), "kind": kind, "models": models,
                "rpm": cfg["rpm"], "max_concurrency": cfg["max_concurrency"]}
        items = [item if p["id"] == connection_id else p for p in items]
        if not old:
            items.append(item)
        path = config.DATA_DIR / "connections.json"
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(items, ensure_ascii=False, indent=2), encoding="utf-8")
        temp.replace(path)
    return next(p for p in public() if p["id"] == connection_id)


def activate(connection_id, model, kind):
    item = next((p for p in entries() if p["id"] == connection_id and p["kind"] == kind), None)
    if not item or not re.fullmatch(r"[a-z0-9-]+", connection_id) or model not in item["models"]:
        raise ValueError("连接或模型不存在")
    cfg = settings.load_settings("profile-" + connection_id)
    # Preserve the user's explicit auto-analysis preference when changing providers.
    value = settings.save_settings({"base_url": cfg["base_url"], "model": model, "api_key": settings.api_key(cfg),
                                   "enabled": True, "rpm": item["rpm"], "max_concurrency": item["max_concurrency"],
                                   "json_mode": cfg.get("json_mode", True), "reference_api": cfg.get("reference_api", "edits"),
                                   "reasoning_effort": cfg.get("reasoning_effort", "")}, kind)
    return value
