"""Versioned local creative preferences; credentials use the existing key store."""
import copy
import json

from .. import config
from ..manga.models import MangaError
from . import settings as credentials

DEFAULTS = {"search": {"enabled": False, "provider": "tavily", "base_url": "https://api.tavily.com",
                       "auto_enrich": False, "max_results": 3, "timeout": 15},
            "memory": {"enabled": True, "max_items": 8, "max_chars": 6000},
            "prompts": {"text": "", "image": "", "negative": ""}, "revision": 0}


def load():
    with credentials._lock:
        path = config.DATA_DIR / "ai-features.json"
        saved = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}
        return {**copy.deepcopy(DEFAULTS), **saved}


def public():
    value = load()
    value["search"]["key_configured"] = credentials.public_settings("web-search")["key_configured"]
    return value


def save(patch):
    if not isinstance(patch, dict) or set(patch) - set(DEFAULTS):
        raise MangaError("功能设置包含未知字段")
    with credentials._lock:
        value = load()
        if "revision" in patch and (type(patch["revision"]) is not int or patch["revision"] != value["revision"]):
            raise MangaError("设置已变化，请刷新后保存", "revision_conflict", 409)
        for section in ("search", "memory", "prompts"):
            if section not in patch:
                continue
            part = patch[section]
            allowed = set(DEFAULTS[section]) | ({"api_key"} if section == "search" else set())
            if not isinstance(part, dict) or set(part) - allowed:
                raise MangaError(section + " 设置字段无效")
            value[section].update({k: v for k, v in part.items() if k != "api_key"})
        search, memory = value["search"], value["memory"]
        for part, fields in ((search, ("enabled", "auto_enrich")), (memory, ("enabled",))):
            for key in fields:
                if type(part[key]) is not bool:
                    raise MangaError(key + " 须为布尔值")
        if search["provider"] not in ("tavily", "searxng", "brave"):
            raise MangaError("搜索服务须为 Tavily、SearXNG 或 Brave")
        try:
            search["base_url"] = credentials.validate_url(search["base_url"])
        except ValueError as exc:
            raise MangaError(str(exc)) from None
        for part, key, low, high in ((search, "max_results", 1, 10), (search, "timeout", 3, 60),
                                     (memory, "max_items", 1, 30), (memory, "max_chars", 500, 16000)):
            if type(part[key]) is not int or not low <= part[key] <= high:
                raise MangaError(key + " 超出有效范围")
        for key, maximum in (("text", 6000), ("image", 3000), ("negative", 2000)):
            text = value["prompts"][key]
            if not isinstance(text, str) or len(text) > maximum or "\x00" in text:
                raise MangaError(key + " 提示词过长或格式无效")
        key_patch = patch.get("search", {})
        old = credentials.load_settings("web-search")
        if search["enabled"] and search["provider"] != "searxng" and not key_patch.get("api_key") and (
                old["base_url"] != search["base_url"] or not credentials.public_settings("web-search")["key_configured"]):
            raise MangaError("请先填写搜索服务密钥")
        if "search" in patch:
            try:
                credentials.save_settings({"base_url": search["base_url"], "api_key": key_patch.get("api_key", ""),
                                           "enabled": False}, "web-search")
            except ValueError as exc:
                raise MangaError(str(exc)) from None
        value["revision"] += 1
        path = config.DATA_DIR / "ai-features.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
        temp.replace(path)
    return public()


def text_system(system):
    supplement = load()["prompts"]["text"].strip()
    return system + ("\n用户创作偏好（补充约束；保持以上原文证据、人物身份和输出结构要求）：\n" + supplement if supplement else "")


def image_prompt(prompt):
    prefs = load()["prompts"]
    return ". ".join(p for p in (prompt, prefs["image"].strip(), "Avoid: " + prefs["negative"].strip() if prefs["negative"].strip() else "") if p)
