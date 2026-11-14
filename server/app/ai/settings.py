"""Local text API settings.

Credentials are saved in the app's local data directory and never enter book
snapshots, routing telemetry, or public API responses.  Legacy Windows DPAPI
values remain readable for migration, while new values use the same local
field on every platform.
"""
from __future__ import annotations

import base64
import ctypes
import ipaddress
import json
import os
import threading
from urllib.parse import urlsplit

from .. import config

_lock = threading.RLock()
DEFAULTS = {"base_url": "", "model": "gpt-oss-120b", "enabled": False, "auto_analyze": True,
            "rpm": 0, "max_concurrency": 1, "json_mode": True, "reference_api": "edits",
            "reasoning_effort": ""}
REASONING_EFFORTS = {"", "minimal", "low", "medium", "high", "max"}


def _path(slot="text"):
    return config.DATA_DIR / (slot + "-api.json")


def _crypt(raw: bytes, decrypt=False) -> bytes:
    from ctypes import wintypes
    class Blob(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("data", ctypes.POINTER(ctypes.c_ubyte))]
    buffer = ctypes.create_string_buffer(raw)
    source = Blob(len(raw), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte)))
    target = Blob()
    crypt = ctypes.windll.crypt32
    if decrypt:
        ok = crypt.CryptUnprotectData(ctypes.byref(source), None, None, None, None, 1, ctypes.byref(target))
    else:
        ok = crypt.CryptProtectData(ctypes.byref(source), "Velmira text API", None, None, None, 1, ctypes.byref(target))
    if not ok:
        raise ValueError("无法读取或保存本机 API 密钥")
    try:
        return ctypes.string_at(target.data, target.size)
    finally:
        ctypes.windll.kernel32.LocalFree(target.data)


def load_settings(slot="text"):
    with _lock:
        try:
            value = json.loads(_path(slot).read_text(encoding="utf-8"))
            return {**DEFAULTS, **value}
        except FileNotFoundError:
            return dict(DEFAULTS)


def api_key(settings=None):
    value = settings if settings is not None else load_settings()
    if value.get("key_dpapi"):
        return _crypt(base64.b64decode(value["key_dpapi"]), decrypt=True).decode("utf-8")
    return value.get("key_local", "")


def api_key_for(base_url, slot="text"):
    value = load_settings(slot)
    if value["base_url"] != base_url:
        raise ValueError("文本 API 地址已变化，请重新发起请求")
    return api_key(value)


def public_settings(slot="text"):
    value = load_settings(slot)
    return {**{k: value[k] for k in DEFAULTS}, "key_configured": bool(value.get("key_dpapi") or value.get("key_local"))}


def _private_host(hostname):
    """Loopback, private LAN and mDNS names may use plain HTTP.

    Everything else must use HTTPS: a remote HTTP endpoint sends the saved API
    key in cleartext. Keeping this explicit also keeps the older rule (remote
    plain HTTP is rejected) while still allowing local Ollama-style servers.
    """
    host = str(hostname or "").strip("[]").lower()
    if host == "localhost" or host.endswith(".localhost") or host.endswith(".local") or host.endswith(".lan"):
        return True
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        return False
    return address.is_loopback or address.is_private or address.is_link_local


def validate_url(value):
    if not isinstance(value, str) or len(value) > 500:
        raise ValueError("API 地址无效")
    value = value.strip().rstrip("/")
    url = urlsplit(value)
    if not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError("API 地址不能包含密钥、查询参数或账号密码")
    try:
        port = url.port
    except ValueError:
        raise ValueError("API 地址端口无效") from None
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("API 地址端口无效")
    # OpenAI-compatible gateways are frequently exposed over plain HTTP on a
    # private LAN (for example, a local Ollama or a home server).  The client
    # still rejects malformed URLs and embedded credentials, but transport
    # security is a deployment choice rather than a reason to reject a valid
    # endpoint here.  ``urlsplit`` normalises the scheme to lower case.
    if url.scheme not in {"http", "https"}:
        raise ValueError("API 地址须使用 HTTP 或 HTTPS")
    if url.scheme == "http" and not _private_host(url.hostname):
        raise ValueError("远程 API 地址须使用 HTTPS；本机或局域网地址可使用 HTTP")
    return value


def save_settings(patch, slot="text"):
    if not isinstance(patch, dict) or set(patch) - {*DEFAULTS, "api_key"}:
        raise ValueError("文本 API 设置包含未知字段")
    with _lock:
        value = load_settings(slot)
        if "base_url" in patch:
            if not isinstance(patch["base_url"], str):
                raise ValueError("API 地址无效")
            raw_address = patch["base_url"].strip().rstrip("/")
            address = validate_url(raw_address) if raw_address else ""
            if address != value["base_url"]:
                value.pop("key_dpapi", None); value.pop("key_local", None)
            value["base_url"] = address
        if "model" in patch:
            if not isinstance(patch["model"], str) or len(patch["model"]) > 200:
                raise ValueError("模型名称须为文本（最多 200 字符）")
            value["model"] = patch["model"].strip()
        if "enabled" in patch:
            if not isinstance(patch["enabled"], bool):
                raise ValueError("enabled 须为布尔值")
            value["enabled"] = patch["enabled"]
        if "auto_analyze" in patch:
            if not isinstance(patch["auto_analyze"], bool):
                raise ValueError("auto_analyze 须为布尔值")
            value["auto_analyze"] = patch["auto_analyze"]
        for field, low, high in [("rpm", 0, 1000), ("max_concurrency", 1, 8)]:
            if field in patch:
                number = patch[field]
                if isinstance(number, bool) or not isinstance(number, int) or not low <= number <= high:
                    raise ValueError(field + " 超出有效范围")
                value[field] = number
        if "json_mode" in patch:
            if not isinstance(patch["json_mode"], bool):
                raise ValueError("json_mode 须为布尔值")
            value["json_mode"] = patch["json_mode"]
        if "reference_api" in patch:
            if patch["reference_api"] not in ("edits", "generations"):
                raise ValueError("参考图接口无效")
            value["reference_api"] = patch["reference_api"]
        if "reasoning_effort" in patch:
            effort = patch["reasoning_effort"]
            if not isinstance(effort, str) or effort.strip().lower() not in REASONING_EFFORTS:
                raise ValueError("推理强度须为空或 minimal/low/medium/high/max")
            value["reasoning_effort"] = effort.strip().lower()
        # Blank leaves the existing credential in place, so reopening the form is safe.
        key = patch.get("api_key", "")
        if not isinstance(key, str) or len(key) > 2000 or any(c in key for c in "\r\n"):
            raise ValueError("API 密钥格式无效")
        key = key.strip()
        if key:
            value.pop("key_dpapi", None); value.pop("key_local", None)
            # Keep the key in the app's local data directory.  Users asked for
            # a normal saved field that can be reused by model-library and
            # translation providers, rather than a separate privacy-key setup
            # flow.  We still read legacy DPAPI values above so existing
            # installations continue to work; new writes use one format on all
            # platforms and never expose the key via public_settings().
            value["key_local"] = key
        if value["enabled"] and (not value["base_url"] or not value["model"]):
            from .routing import is_enabled
            if slot != "text" or not is_enabled("text"):
                raise ValueError("启用前请填写 API 地址和模型，或配置文本模型池")
        path = _path(slot); path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(".tmp")
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False)
        temp.replace(path)
    return public_settings(slot)
