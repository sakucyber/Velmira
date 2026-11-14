"""Safe provider failure classification for UI diagnostics.

Only stable categories and HTTP status classes leave the backend. Upstream
response bodies, URLs and credentials are deliberately ignored.
"""
from __future__ import annotations
import re


def provider_failure(exc) -> str:
    """Map an exception to a stable, non-sensitive diagnostic code."""
    status = getattr(exc, "status_code", None)
    try:
        status = int(status) if status is not None else None
    except (TypeError, ValueError):
        status = None
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


_FAILURE_CODES = {"provider_auth", "provider_forbidden", "provider_billing", "provider_rate_limit",
                  "provider_server_error", "provider_timeout", "provider_offline", "provider_invalid_response", "provider_failed"}


def stored_failure(message: str) -> str | None:
    """Read only our fixed token from a stored safe error, never display text."""
    token = re.search(r"\[(provider_[a-z_]+)\]", str(message or ""))
    return token[1] if token and token[1] in _FAILURE_CODES else None


def public_failure(code: str) -> dict[str, str]:
    """Return user-facing copy for a stable diagnostic code."""
    title, detail, kind = {
        "provider_auth": ("API 密钥无效或已过期", "检查模型库中的 API Key 和接口地址后重试。", "credentials"),
        "provider_forbidden": ("API 访问被拒绝", "检查账号权限、模型权限和接口白名单。", "credentials"),
        "provider_billing": ("API 账户余额或额度不足", "检查供应商余额、套餐额度或计费状态。", "billing"),
        "provider_rate_limit": ("API 请求过于频繁", "等待限流窗口结束，或启用备用模型并降低并发。", "rate_limit"),
        "provider_server_error": ("AI 服务暂时故障", "供应商返回了服务端错误，稍后重试或切换备用模型。", "server"),
        "provider_timeout": ("AI 服务响应超时", "检查网络和超时设置，或切换响应更快的备用模型。", "network"),
        "provider_offline": ("无法连接 AI 服务", "检查网络、接口地址和本机服务是否启动。", "network"),
        "provider_invalid_response": ("AI 服务返回格式无效", "切换模型或恢复默认提示词后重试。", "response"),
        "provider_failed": ("AI 服务请求失败", "检查模型连接后重试，必要时切换备用模型。", "general"),
    }.get(code, ("AI 服务请求失败", "检查模型连接后重试，必要时切换备用模型。", "general"))
    return {"code": code, "title": title, "detail": detail, "kind": kind}

