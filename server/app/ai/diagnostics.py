"""Safe provider failure classification, actionable copy and bounded samples.

Only stable categories, HTTP status classes and token counts leave the backend
or the local diagnostic file. Upstream response bodies, prompts, URLs and
credentials are deliberately never stored or returned.
"""
from __future__ import annotations

import json
import re
import time

from .. import config

STAGE_LABELS = {
    "analysis": "章节分析",
    "reduction": "章节大意合并",
    "memory": "章节记忆",
    "probe": "连接检测",
    "diagnose": "诊断",
}

_FAILURE_CODES = {"provider_auth", "provider_forbidden", "provider_billing", "provider_rate_limit",
                  "provider_server_error", "provider_timeout", "provider_offline", "provider_invalid_response", "provider_failed"}

_SECRET_PATTERNS = (
    re.compile(r"sk-[A-Za-z0-9_\-]{8,}"),
    re.compile(r"(?i)bearer\s+[A-Za-z0-9._\-]{8,}"),
    re.compile(r"(?i)(api[_-]?key|authorization|access[_-]?token|secret)\s*[:=]\s*[\"']?[^\"'\s,}]{8,}"),
)


def _status(exc):
    status = getattr(exc, "status_code", None)
    try:
        status = int(status) if status is not None else None
    except (TypeError, ValueError):
        status = None
    return status if status is not None and 100 <= status <= 599 else None


def provider_failure(exc) -> str:
    """Map an exception to a stable, non-sensitive diagnostic code."""
    status = _status(exc)
    if status == 401:
        return "provider_auth"
    if status == 403:
        return "provider_forbidden"
    if status == 402:
        return "provider_billing"
    if status == 429:
        return "provider_rate_limit"
    if status is not None and 500 <= status <= 599:
        return "provider_server_error"

    code = getattr(exc, "error_code", None) or getattr(exc, "code", None)
    code = str(code or "")
    if code in _FAILURE_CODES:
        return code
    if isinstance(exc, TimeoutError):
        return "provider_timeout"
    if code in {"timeout", "provider_timeout"}:
        return "provider_timeout"
    if code in {"connection_error", "provider_offline", "comfy_unavailable"}:
        return "provider_offline"
    if code in {"capacity", "rate_limit", "provider_rate_limit"}:
        return "provider_rate_limit"
    if code in {"invalid_response", "invalid_text_output"}:
        return "provider_invalid_response"
    if code in {"http_error", "provider_server_error"}:
        return "provider_server_error"
    return "provider_failed"


def failure_reason(exc) -> str:
    """Return a stable reason token (json_invalid, quote_mismatch, ...)."""
    explicit = getattr(exc, "reason", None)
    if isinstance(explicit, str) and explicit:
        return explicit
    if isinstance(exc, TimeoutError):
        return "timeout"
    code = str(getattr(exc, "error_code", None) or getattr(exc, "code", None) or "")
    provider_code = provider_failure(exc)
    reason = {
        "provider_auth": "auth",
        "provider_forbidden": "forbidden",
        "provider_billing": "billing",
        "provider_rate_limit": "rate_limited",
        "provider_server_error": "server_error",
        "provider_timeout": "timeout",
        "provider_offline": "offline",
        "provider_invalid_response": "invalid_response",
        "timeout": "timeout",
        "connection_error": "offline",
        "empty_content": "empty_content",
        "output_truncated": "output_truncated",
        "reasoning_budget": "reasoning_budget",
        "invalid_json": "json_invalid",
        "json_invalid": "json_invalid",
        "invalid_config": "config_invalid",
        "invalid_memory_output": "field_invalid",
        "invalid_text_output": "field_invalid",
    }.get(provider_code) or {
        "timeout": "timeout",
        "empty_content": "empty_content",
        "output_truncated": "output_truncated",
        "reasoning_budget": "reasoning_budget",
        "invalid_json": "json_invalid",
        "json_invalid": "json_invalid",
        "invalid_config": "config_invalid",
        "invalid_memory_output": "field_invalid",
        "invalid_text_output": "field_invalid",
    }.get(code)
    if reason:
        return reason
    if getattr(exc, "finish_reason", None) == "length":
        return "output_truncated"
    return "unknown"


_REASON_COPY = {
    "auth": ("API 密钥无效或已过期", "检查模型库中的 API Key 和接口地址后重试。"),
    "forbidden": ("API 访问被拒绝", "检查账号权限、模型权限和接口白名单。"),
    "billing": ("API 账户余额或额度不足", "检查供应商余额、套餐额度或计费状态。"),
    "rate_limited": ("API 请求过于频繁", "等待限流窗口结束，或启用备用模型并降低并发。"),
    "server_error": ("AI 服务暂时故障", "供应商返回了服务端错误，稍后重试或切换备用模型。"),
    "timeout": ("AI 服务响应超时", "检查网络和超时设置，或切换响应更快的备用模型。"),
    "offline": ("无法连接 AI 服务", "检查网络、接口地址和本机服务是否启动。"),
    "invalid_response": ("AI 服务返回格式无效", "切换模型或恢复默认提示词后重试。"),
    "empty_content": ("模型返回空内容", "提高最大 Token、换用非推理模型，或稍后重试。"),
    "output_truncated": ("输出达到最大 Token 被截断", "提高最大 Token 或缩短单次输入后重试。"),
    "reasoning_budget": ("推理模型把输出预算用在了思考过程", "把最大 Token 提高到 16000 以上，或降低 reasoning effort。"),
    "json_invalid": ("模型没有返回有效 JSON", "恢复默认提示词、切换模型，或确认服务支持 JSON 输出。"),
    "field_invalid": ("模型输出字段不符合要求", "切换模型或恢复默认提示词后重试。"),
    "quote_mismatch": ("原文引文与章节正文不一致", "切换引文更准确的模型；系统已保留可验证的事实并丢弃其余条目。"),
    "config_invalid": ("本地 AI 配置无效", "检查最大 Token、连接和模型设置后重试；配置错误不会重复请求。"),
    "unknown": ("文本请求最多尝试 3 次仍失败", "打开诊断查看阶段与原因，或切换模型池中的其他模型。"),
}


def public_failure(code: str) -> dict[str, str]:
    """Return user-facing copy for a stable diagnostic code."""
    title, detail, kind = {
        "provider_auth": (_REASON_COPY["auth"][0], _REASON_COPY["auth"][1], "credentials"),
        "provider_forbidden": (_REASON_COPY["forbidden"][0], _REASON_COPY["forbidden"][1], "credentials"),
        "provider_billing": (_REASON_COPY["billing"][0], _REASON_COPY["billing"][1], "billing"),
        "provider_rate_limit": (_REASON_COPY["rate_limited"][0], _REASON_COPY["rate_limited"][1], "rate_limit"),
        "provider_server_error": (_REASON_COPY["server_error"][0], _REASON_COPY["server_error"][1], "server"),
        "provider_timeout": (_REASON_COPY["timeout"][0], _REASON_COPY["timeout"][1], "network"),
        "provider_offline": (_REASON_COPY["offline"][0], _REASON_COPY["offline"][1], "network"),
        "provider_invalid_response": (_REASON_COPY["invalid_response"][0], _REASON_COPY["invalid_response"][1], "response"),
        "provider_failed": (_REASON_COPY["unknown"][0], _REASON_COPY["unknown"][1], "general"),
    }.get(code, (_REASON_COPY["unknown"][0], _REASON_COPY["unknown"][1], "general"))
    return {"code": code, "title": title, "detail": detail, "kind": kind}


def _token_usage(exc) -> dict:
    usage = getattr(exc, "usage", None)
    if not isinstance(usage, dict):
        return {}
    details = usage.get("completion_tokens_details")
    reasoning = details.get("reasoning_tokens") if isinstance(details, dict) else None
    if reasoning is None:
        # ProviderRequestError flattens the details into stable top-level counts.
        reasoning = usage.get("reasoning_tokens")
    result = {}
    for key in ("prompt_tokens", "completion_tokens", "total_tokens"):
        value = usage.get(key)
        if type(value) is int and value >= 0:
            result[key] = value
    if type(reasoning) is int and reasoning >= 0:
        result["reasoning_tokens"] = reasoning
    return result


def failure_detail(exc, *, stage=None, chapter=None, model=None) -> dict:
    """Build the public failure object shown by the analysis panel.

    The object intentionally carries categories and token counts only.  It is
    safe to persist in SQLite and to return from an API response.
    """
    reason = failure_reason(exc)
    status = _status(exc)
    finish = getattr(exc, "finish_reason", None)
    finish = finish if isinstance(finish, str) and len(finish) <= 40 else None
    usage = _token_usage(exc)
    max_tokens = getattr(exc, "max_tokens", None)
    max_tokens = max_tokens if type(max_tokens) is int and max_tokens > 0 else None
    title, suggestion = _REASON_COPY.get(reason, _REASON_COPY["unknown"])
    provider_code = provider_failure(exc)
    code = provider_code if provider_code != "provider_failed" else {
        "json_invalid": "output_json_invalid",
        "field_invalid": "output_field_invalid",
        "quote_mismatch": "output_quote_mismatch",
        "empty_content": "output_empty",
        "output_truncated": "output_truncated",
        "reasoning_budget": "output_reasoning_budget",
        "config_invalid": "config_invalid",
    }.get(reason, "analysis_failed")
    stage_label = STAGE_LABELS.get(stage or "", stage or "AI")
    prefix = ("第 " + str(int(chapter) + 1) + " 章") if isinstance(chapter, int) and chapter >= 0 else ""
    detail = title
    if reason in {"empty_content", "output_truncated", "reasoning_budget"}:
        parts = []
        if finish:
            parts.append("finish_reason=" + finish)
        if usage:
            used = usage.get("completion_tokens")
            if used is not None:
                parts.append("已用 " + str(used) + ("/" + str(max_tokens) if max_tokens else "") + " token")
            if usage.get("reasoning_tokens"):
                parts.append("思考 " + str(usage["reasoning_tokens"]) + " token")
        if parts:
            detail += "（" + "，".join(parts) + "）"
    if status is not None:
        detail += "（HTTP " + str(status) + "）"
    message = prefix + stage_label + "阶段失败：" + detail
    if isinstance(model, str) and model:
        message += " · 模型 " + model[:120]
    return {"code": code, "reason": reason, "stage": stage, "chapter": chapter,
            "model": model if isinstance(model, str) else None,
            "http_status": status, "finish_reason": finish, "usage": usage,
            "max_tokens": max_tokens, "message": message[:600], "title": title,
            "suggestion": suggestion}


def non_retryable(exc) -> bool:
    """Configuration problems will not become successes by retrying."""
    code = str(getattr(exc, "error_code", None) or "")
    if code in {"invalid_config", "provider_auth", "provider_forbidden", "provider_billing"}:
        return True
    return provider_failure(exc) in {"provider_auth", "provider_forbidden", "provider_billing"}


def stored_failure(message: str) -> str | None:
    """Read only our fixed token from a stored safe error, never display text."""
    token = re.search(r"\[(provider_[a-z_]+)\]", str(message or ""))
    return token[1] if token and token[1] in _FAILURE_CODES else None


def _redact(text: str) -> str:
    for pattern in _SECRET_PATTERNS:
        text = pattern.sub("[redacted]", text)
    return text


def _safe_name(value) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", str(value or ""))[:80] or "unknown"


def sample_path(book_id, stage):
    return config.DATA_DIR / "diagnostics" / ("analysis-" + _safe_name(book_id) + "-" + _safe_name(stage) + ".json")


def save_sample(book_id, stage, raw, detail=None) -> None:
    """Persist one bounded, redacted failure sample per book and stage.

    A single file per stage is enough to diagnose the last failure and keeps
    the local data directory from growing without bound.
    """
    try:
        if isinstance(raw, (dict, list)):
            text = json.dumps(raw, ensure_ascii=False)[:16000]
        else:
            text = str(raw or "")[:16000]
        payload = {"created_at": int(time.time()), "stage": stage, "detail": detail or {},
                   "sample": _redact(text)}
        path = sample_path(book_id, stage)
        path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
        temp.replace(path)
    except (OSError, TypeError, ValueError):
        # Diagnostics must never turn an AI failure into a second failure.
        pass


def samples(book_id) -> list[dict]:
    folder = config.DATA_DIR / "diagnostics"
    result = []
    for path in sorted(folder.glob("analysis-" + _safe_name(book_id) + "-*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        result.append({"stage": payload.get("stage"), "created_at": payload.get("created_at"),
                       "reason": (payload.get("detail") or {}).get("reason"),
                       "size": path.stat().st_size, "name": path.name})
    return result[:20]


def read_sample(book_id, name) -> dict | None:
    """Read one of our own sample files, refusing any path traversal."""
    if not isinstance(name, str) or "/" in name or "\\" in name or not name.endswith(".json"):
        return None
    if not name.startswith("analysis-" + _safe_name(book_id) + "-"):
        return None
    path = config.DATA_DIR / "diagnostics" / name
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return payload if isinstance(payload, dict) else None
