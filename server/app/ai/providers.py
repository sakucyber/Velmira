"""可配置文本 provider：默认 Mock，另提供 OpenAI-compatible HTTP 实现。"""
from __future__ import annotations

import asyncio
import json
import os
import re
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Protocol

from .models import CARD_FIELDS


class TextProvider(Protocol):
    async def complete(self, *, system: str, user: str, schema: dict | None = None, temperature: float = 0.2) -> dict | str:
        ...


def _first_sentence(text: str, limit: int = 180) -> str:
    text = re.sub(r"\s+", " ", text.strip())
    match = re.search(r"(.{1,180}?[。！？!?])", text)
    return (match.group(1) if match else text[:limit]).strip()


def _candidate_names(text: str) -> list[tuple[str, str]]:
    """提取带叙述动词的中英文姓名，避免把整段普通中文词语当角色。"""
    found: list[tuple[str, str]] = []
    patterns = [
        r"([\u4e00-\u9fff]{2,4})(?=(?:说道|问道|喊道|叫道|笑道|看见|走来|走进|点头|摇头))",
        r"([A-Z][a-z]{1,20})(?= said| asked| looked| walked| smiled)",
    ]
    for pattern in patterns:
        for match in re.finditer(pattern, text):
            name = match.group(1).split("的")[-1]
            if len(name) < 2:
                continue
            start = max(0, match.start() - 45)
            end = min(len(text), match.end() + 15)
            quote = text[start:end].strip()
            if not any(name == old for old, _ in found):
                found.append((name, quote))
    return found[:12]


def _visual_fields(text: str, name: str, quote: str, chapter: int) -> tuple[dict[str, str], list[dict[str, Any]]]:
    fields: dict[str, str] = {}
    evidence: list[dict[str, Any]] = []
    # 只认明确的外貌/服饰模式，全部证据必须来自原文。
    patterns = {
        "hair_color": r"((?:黑|白|棕|金|红|银|蓝|紫)色(?:的)?(?:头发|长发|短发))",
        "hair_style": r"((?:短发|长发|卷发|马尾|双马尾|寸头|披肩发))",
        "eye_color": r"((?:黑|蓝|绿|棕|金|灰|紫)色(?:的)?眼睛)",
        "outfit": r"((?:穿着|身穿|披着|套着)[^，。！？；]{1,24})",
        "accessories": r"((?:戴着|佩戴)[^，。！？；]{1,20})",
    }
    for field, pattern in patterns.items():
        match = re.search(pattern, text)
        if not match:
            continue
        value = match.group(1).strip()
        fields[field] = value
        quote_start = max(0, match.start() - 20)
        quote_end = min(len(text), match.end() + 20)
        evidence.append({"field": field, "value": value, "quote": text[quote_start:quote_end].strip(), "ch": chapter})
    return fields, evidence


@dataclass(frozen=True)
class MockTextProvider:
    """离线、确定性 provider；不模拟不存在的视觉字段。"""

    name: str = "mock"
    model: str = "deterministic-v1"

    async def complete(self, *, system: str, user: str, schema: dict | None = None, temperature: float = 0.2) -> dict:
        del system, schema, temperature
        text_match = re.search(r"片段：\s*\n([\s\S]*)$", user)
        text = text_match.group(1).strip() if text_match else user
        chapter_match = re.search(r"章节：\s*(-?\d+)", user)
        chapter = int(chapter_match.group(1)) if chapter_match else 0
        names = _candidate_names(text)
        characters = []
        for name, quote in names:
            card, evidence = _visual_fields(text, name, quote, chapter)
            characters.append({"name": name, "aliases": [], "card": card, "evidence": evidence})
        location = ""
        for term in ("教室", "房间", "客厅", "街道", "森林", "城堡", "医院", "车站", "山上", "河边"):
            if term in text:
                location = term
                break
        time_of_day = next((term for term in ("清晨", "早晨", "中午", "黄昏", "夜里", "深夜") if term in text), "")
        props = [term for term in ("剑", "刀", "枪", "书", "手机", "钥匙", "灯") if term in text]
        return {
            "kind": "scene",
            "summary": _first_sentence(text),
            "location": location,
            "time_of_day": time_of_day,
            "characters": characters,
            "key_props": props,
            "drawable": 0.8 if text else 0.0,
            "reason": "片段包含可视化动作、场景或人物信息" if text else "片段为空",
            "continuity": "",
        }


@dataclass(frozen=True)
class OpenAICompatProvider:
    """兼容 OpenAI chat/completions 的 provider。

    api_key_env 只保存环境变量名；密钥本身永远不进入配置快照或数据库。
    """

    base_url: str
    model: str
    api_key_env: str = ""
    name: str = "openai-compatible"

    async def complete(self, *, system: str, user: str, schema: dict | None = None, temperature: float = 0.2) -> dict | str:
        return await asyncio.to_thread(self._complete_sync, system, user, schema, temperature)

    def _complete_sync(self, system: str, user: str, schema: dict | None, temperature: float) -> dict | str:
        url = self.base_url.rstrip("/")
        if not url.endswith("/chat/completions"):
            url += "/chat/completions"
        body: dict[str, Any] = {
            "model": self.model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": max(0.0, min(2.0, float(temperature))),
        }
        if schema:
            body["response_format"] = {"type": "json_object"}
        request = urllib.request.Request(url, data=json.dumps(body, ensure_ascii=False).encode("utf-8"), method="POST")
        request.add_header("Content-Type", "application/json")
        api_key = os.environ.get(self.api_key_env, "") if self.api_key_env else ""
        if api_key:
            request.add_header("Authorization", f"Bearer {api_key}")
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                payload = json.loads(response.read().decode("utf-8"))
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise RuntimeError(f"文本 provider 请求失败：{exc}") from exc
        try:
            content = payload["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError) as exc:
            raise RuntimeError("文本 provider 返回缺少 choices[0].message.content") from exc
        if isinstance(content, str):
            stripped = content.strip()
            try:
                return json.loads(stripped)
            except json.JSONDecodeError:
                return stripped
        return content


def make_provider(config: dict[str, Any], requested: str | None = None) -> TextProvider:
    provider = (requested or config.get("provider") or "mock").strip().lower()
    if provider in {"mock", "offline"}:
        return MockTextProvider()
    if provider in {"openai", "openai-compatible", "openai_compat", "ollama", "llamacpp"}:
        base_url = str(config.get("base_url") or "").strip()
        model = str(config.get("model") or "").strip()
        if not base_url or not model:
            raise ValueError("openai-compatible provider 需要 base_url 和 model")
        return OpenAICompatProvider(base_url=base_url, model=model, api_key_env=str(config.get("api_key_env") or ""))
    raise ValueError(f"不支持的文本 provider: {provider}")


# 架构文档中的简短 provider 名称，便于外部集成保持稳定导入路径。
OpenAICompat = OpenAICompatProvider
Ollama = OpenAICompatProvider
LlamaCpp = OpenAICompatProvider
Mock = MockTextProvider

