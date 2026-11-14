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

GPT_OSS_CONTEXT_TOKENS = 128000
MAX_OUTPUT_TOKENS = 6000


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class ProviderRequestError(RuntimeError):
    """A safe failure category; never retain an upstream response body or key."""

    def __init__(self, message, *, status_code=None, error_code="request_failed"):
        super().__init__(message)
        self.status_code = status_code
        self.error_code = error_code


def request_json(url, *, api_key="", body=None, timeout=120):
    request = urllib.request.Request(url, data=json.dumps(body, ensure_ascii=False).encode("utf-8") if body is not None else None)
    request.add_header("Content-Type", "application/json")
    request.add_header("User-Agent", "Velmira/0.4")
    if api_key:
        request.add_header("Authorization", "Bearer " + api_key)
    try:
        with urllib.request.build_opener(_NoRedirect()).open(request, timeout=timeout) as response:
            raw = response.read(4 * 1024 * 1024 + 1)
            if len(raw) > 4 * 1024 * 1024:
                raise ProviderRequestError("文本 API 返回内容过大", error_code="invalid_response")
            return json.loads(raw.decode("utf-8"))
    except urllib.error.HTTPError as exc:
        # Do not persist arbitrary gateway error bodies, which may echo credentials.
        raise ProviderRequestError(f"文本 API 请求失败（HTTP {exc.code}）", status_code=exc.code,
                                   error_code="http_error") from None
    except (TimeoutError, urllib.error.URLError, OSError) as exc:
        timed_out = isinstance(exc, TimeoutError) or isinstance(getattr(exc, "reason", None), TimeoutError)
        raise ProviderRequestError("文本 API 请求超时" if timed_out else "文本 API 连接失败，请检查连接、模型和密钥",
                                   error_code="timeout" if timed_out else "connection_error") from None
    except ValueError:
        raise ProviderRequestError("文本 API 返回格式无效", error_code="invalid_response") from None


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
    local_settings: bool = False

    async def complete(self, *, system: str, user: str, schema: dict | None = None, temperature: float = 0.2) -> dict | str:
        return await asyncio.to_thread(self._complete_sync, system, user, schema, temperature)

    async def complete_validated(self, *, validate, system: str, user: str,
                                 schema: dict | None = None, temperature: float = 0.2) -> dict | str:
        """Validate routed candidates before committing to one model's response.

        The caller retains the final normalization step. Validators used here
        check response data and may update an in-memory correction prompt, but
        must not persist application state.
        """
        return await asyncio.to_thread(self._complete_sync, system, user, schema, temperature, validate)

    def _complete_sync(self, system: str, user: str, schema: dict | None, temperature: float, validate=None) -> dict | str:
        if not isinstance(system, str) or not isinstance(user, str):
            raise ValueError("文本模型仅接受文字输入")
        if self.local_settings:
            from .features import text_system
            system = text_system(system)
        if self.local_settings:
            from . import routing
            if routing.is_enabled("text"):
                def invoke(cfg):
                    raw = self._complete_once(system, user, schema, temperature, cfg)
                    if validate is not None:
                        try:
                            validate(raw)
                        except Exception:
                            raise ProviderRequestError("文本 API 输出未通过结构校验", error_code="invalid_response") from None
                    return raw
                return routing.run("text", invoke)
            from .settings import load_settings
            cfg = {**load_settings(), "base_url": self.base_url, "model": self.model, "credential_slot": "text"}
        else:
            cfg = {"base_url": self.base_url, "model": self.model}
        return self._complete_once(system, user, schema, temperature, cfg)

    def _complete_once(self, system, user, schema, temperature, cfg):
        model = cfg["model"]
        max_tokens = cfg.get("max_tokens", MAX_OUTPUT_TOKENS)
        # UTF-8 byte count is a conservative upper bound for this byte-level tokenizer.
        # Reserve output and message framing without requiring tokenizer weights locally.
        if model.lower().split("/")[-1] == "gpt-oss-120b":
            input_bound = len(system.encode("utf-8")) + len(user.encode("utf-8")) + 1024
            if input_bound + max_tokens > GPT_OSS_CONTEXT_TOKENS:
                raise ValueError("文本上下文超过 gpt-oss-120b 的保守 128K 预算，请缩小片段范围")
        url = cfg["base_url"].rstrip("/")
        if not url.endswith("/chat/completions"):
            url += "/chat/completions"
        body: dict[str, Any] = {
            "model": model,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "temperature": max(0.0, min(2.0, float(cfg.get("temperature", temperature)))),
            "max_tokens": max_tokens,
        }
        if schema:
            body["response_format"] = {"type": "json_object"}
        if self.local_settings:
            from .settings import api_key_for
            key = api_key_for(cfg["base_url"], cfg.get("credential_slot", "text"))
        else:
            key = os.environ.get(self.api_key_env, "") if self.api_key_env else ""
        from .limits import request_slot
        if not cfg.get("json_mode", True):
            body.pop("response_format", None)
        with request_slot((cfg["base_url"], "text"), cfg.get("rpm", 0), cfg.get("max_concurrency", 1),
                          wait_timeout=0 if cfg.get("_route_id") else None):
            payload = request_json(url, api_key=key, body=body, timeout=cfg.get("timeout", 120))
        try:
            content = payload["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError):
            raise ProviderRequestError("文本 provider 返回缺少 choices[0].message.content", error_code="invalid_response") from None
        if isinstance(content, str):
            stripped = content.strip()
            if not stripped:
                raise ProviderRequestError("文本 API 返回空内容", error_code="invalid_response")
            try:
                result = json.loads(stripped)
            except json.JSONDecodeError:
                if schema and cfg.get("_route_id"):
                    raise ProviderRequestError("文本 API 未返回有效 JSON", error_code="invalid_response") from None
                return stripped
            if schema and cfg.get("_route_id") and not isinstance(result, dict):
                raise ProviderRequestError("文本 API 未返回 JSON 对象", error_code="invalid_response")
            return result
        if not isinstance(content, dict) or not content:
            raise ProviderRequestError("文本 API 返回内容格式无效", error_code="invalid_response")
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
        return OpenAICompatProvider(base_url=base_url, model=model, api_key_env=str(config.get("api_key_env") or ""), local_settings=bool(config.get("local_settings")))
    raise ValueError(f"不支持的文本 provider: {provider}")


# 架构文档中的简短 provider 名称，便于外部集成保持稳定导入路径。
OpenAICompat = OpenAICompatProvider
Ollama = OpenAICompatProvider
LlamaCpp = OpenAICompatProvider
Mock = MockTextProvider

