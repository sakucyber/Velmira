"""M2 的可持久化 JSON 模型和确定性规范化函数。

这里不依赖 pydantic：桌面自包含包与离线 CI 只需要项目已有的 FastAPI/stdlib。
所有来自 provider 的值都先经过本模块过滤，避免模型输出直接成为绘图身份提示词。
"""
from __future__ import annotations

import hashlib
import json
import re
from typing import Any


CARD_FIELDS = (
    "gender",
    "age",
    "height_build",
    "hair_style",
    "hair_color",
    "hair_length",
    "eye_shape",
    "eye_color",
    "skin",
    "facial_features",
    "outfit",
    "outfit_variants",
    "accessories",
    "distinctive_features",
    "palette",
)

_EVIDENCE_FIELDS = set(CARD_FIELDS)
_MAX_TEXT = 500
_KIND_VALUES = {"scene", "transition", "montage"}


class AIModelError(ValueError):
    """provider 返回不能安全落库的结构化结果。"""


def _text(value: Any, limit: int = _MAX_TEXT) -> str:
    if value is None:
        return ""
    value = str(value).strip()
    return value[:limit]


def _list(value: Any, limit: int = 20) -> list[str]:
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, (list, tuple)):
        return []
    result = []
    for item in value:
        item = _text(item, 120)
        if item and item not in result:
            result.append(item)
        if len(result) >= limit:
            break
    return result


def _number(value: Any, default: float = 0.0) -> float:
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    try:
        number = float(value)
    except (TypeError, ValueError):
        return default
    return max(0.0, min(1.0, number))


def normalize_evidence(value: Any, source_text: str, chapter: int) -> list[dict[str, Any]]:
    """保留 quote 确实出现在片段中的证据；无证据字段会被丢弃。"""
    if not isinstance(value, list):
        return []
    result: list[dict[str, Any]] = []
    for item in value:
        if not isinstance(item, dict):
            continue
        field = _text(item.get("field"), 60)
        field = field if field in _EVIDENCE_FIELDS else ""
        value_text = _text(item.get("value"), 200)
        quote = _text(item.get("quote"), 500)
        if not field or not value_text or not quote or quote not in source_text:
            continue
        ch = item.get("ch", chapter)
        try:
            ch = int(ch)
        except (TypeError, ValueError):
            ch = chapter
        row = {"field": field, "value": value_text, "quote": quote, "ch": ch}
        if row not in result:
            result.append(row)
    return result


def normalize_scene(value: Any, source_text: str, chapter: int = 0) -> dict[str, Any]:
    """校验并规范化一个 scene JSON。

    provider 可以额外返回字段，但只保留 M2 约定字段。characters 只保留名字、别名和
    evidence/card，最终的绘图 prompt 由后续确定性代码生成。
    """
    if not isinstance(value, dict):
        raise AIModelError("scene 必须是 JSON 对象")
    kind = _text(value.get("kind") or "scene", 20).lower()
    if kind not in _KIND_VALUES:
        raise AIModelError("scene.kind 必须是 scene、transition 或 montage")
    text = _text(source_text, 100000)
    raw_chars = value.get("characters", [])
    if not isinstance(raw_chars, list):
        raw_chars = []
    characters: list[dict[str, Any]] = []
    for item in raw_chars[:12]:
        if isinstance(item, str):
            item = {"name": item}
        if not isinstance(item, dict):
            continue
        name = _text(item.get("name"), 80)
        if not name:
            continue
        card_raw = item.get("card") if isinstance(item.get("card"), dict) else {}
        card = {field: _text(card_raw.get(field), 200) for field in CARD_FIELDS if _text(card_raw.get(field))}
        aliases = _list(item.get("aliases"), 8)
        evidence = normalize_evidence(item.get("evidence"), text, chapter)
        # Identity fields may only enter the card with a matching quote.
        evidence_fields = {row["field"] for row in evidence}
        card = {key: val for key, val in card.items() if key in evidence_fields}
        characters.append({"name": name, "aliases": aliases, "card": card, "evidence": evidence})
    # stable de-duplication by normalized name
    unique: dict[str, dict[str, Any]] = {}
    for character in characters:
        key = normalize_name(character["name"])
        if key not in unique:
            unique[key] = character
        else:
            old = unique[key]
            old["aliases"] = _list(old["aliases"] + character["aliases"], 8)
            old["evidence"] = normalize_evidence(old["evidence"] + character["evidence"], text, chapter)
            for field, val in character["card"].items():
                old["card"].setdefault(field, val)
    return {
        "kind": kind,
        "summary": _text(value.get("summary"), 1000),
        "location": _text(value.get("location"), 200),
        "time_of_day": _text(value.get("time_of_day"), 100),
        "characters": list(unique.values()),
        "key_props": _list(value.get("key_props"), 20),
        "drawable": _number(value.get("drawable"), 0.0),
        "reason": _text(value.get("reason"), 500),
        "continuity": _text(value.get("continuity"), 500),
    }


def normalize_character(value: Any, source_text: str = "", chapter: int = 0) -> dict[str, Any]:
    """将单个人物候选规范化；无 name 的结果拒绝落库。"""
    if not isinstance(value, dict):
        raise AIModelError("character 必须是 JSON 对象")
    name = _text(value.get("name"), 80)
    if not name:
        raise AIModelError("character.name 不能为空")
    card_raw = value.get("card") if isinstance(value.get("card"), dict) else {}
    evidence = normalize_evidence(value.get("evidence"), source_text, chapter)
    evidence_fields = {row["field"] for row in evidence}
    card = {
        field: _text(card_raw.get(field), 200)
        for field in CARD_FIELDS
        if _text(card_raw.get(field)) and field in evidence_fields
    }
    return {
        "name": name,
        "aliases": _list(value.get("aliases"), 8),
        "card": card,
        "evidence": evidence,
    }


def normalize_name(value: str) -> str:
    """用于同名/别名匹配的稳定 key。"""
    value = re.sub(r"[\s·•・.,，。:：;；!?！？'\"“”‘’（）()\[\]{}]", "", _text(value, 80)).lower()
    return value


def merge_card(existing: dict[str, Any], incoming: dict[str, Any], *, frozen: bool = False) -> dict[str, Any]:
    """合并人物卡：冻结后身份字段不覆盖，只追加 evidence/variants。"""
    old = dict(existing) if isinstance(existing, dict) else {}
    old_evidence = old.get("evidence") if isinstance(old.get("evidence"), list) else []
    new_evidence = incoming.get("evidence") if isinstance(incoming.get("evidence"), list) else []
    old["evidence"] = old_evidence + [e for e in new_evidence if e not in old_evidence]
    old_variants = old.get("variants") if isinstance(old.get("variants"), list) else []
    for variant in incoming.get("variants", []) if isinstance(incoming.get("variants"), list) else []:
        if variant not in old_variants:
            old_variants.append(variant)
    if old_variants:
        old["variants"] = old_variants[:30]
    for field in CARD_FIELDS:
        incoming_value = incoming.get(field)
        if not incoming_value:
            continue
        if frozen:
            # outfit changes / growth are versioned as variants; identity stays fixed.
            if field in {"outfit", "outfit_variants", "age", "height_build"}:
                variants = old.setdefault("variants", [])
                value = {"field": field, "value": incoming_value}
                if value not in variants:
                    variants.append(value)
            continue
        if not old.get(field):
            old[field] = incoming_value
    return old


def render_identity(card: dict[str, Any], name: str = "character", chapter: int | None = None) -> str:
    """按固定顺序生成给图像模型使用的英文身份块。

    缺少字段时省略对应片段，绝不编造视觉设定。
    """
    card = card if isinstance(card, dict) else {}
    parts: list[str] = []
    age = f"{_text(card['age'], 50)}-year-old" if card.get("age") else ""
    gender = _text(card["gender"], 50) if card.get("gender") else ""
    if age or gender:
        parts.append(" ".join(part for part in (age, gender) if part))
    if card.get("hair_style") or card.get("hair_color") or card.get("hair_length"):
        hair = " ".join(str(card.get(key, "")).strip() for key in ("hair_style", "hair_length", "hair_color") if card.get(key))
        parts.append(f"{hair} hair")
    if card.get("eye_shape") or card.get("eye_color"):
        eyes = " ".join(str(card.get(key, "")).strip() for key in ("eye_shape", "eye_color") if card.get(key))
        parts.append(f"{eyes} eyes")
    if card.get("skin"):
        parts.append(f"{_text(card['skin'], 80)} skin")
    if card.get("outfit"):
        parts.append(f"wearing {_text(card['outfit'], 160)}")
    accessories = card.get("accessories")
    if isinstance(accessories, (list, tuple)):
        accessories = ", ".join(_list(accessories, 8))
    if accessories:
        parts.append(f"with {_text(accessories, 160)}")
    label = _text(name, 80) or "character"
    return f"{label}, " + ", ".join(parts) if parts else label


def identity_hash(identity_block: str) -> str:
    return hashlib.sha256(_text(identity_block, 4000).encode("utf-8")).hexdigest()


def stable_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
