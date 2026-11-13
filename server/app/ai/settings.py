"""Local text API settings. Credentials never enter book snapshots or API responses."""
from __future__ import annotations

import base64
import ctypes
import json
import os
import threading
from urllib.parse import urlsplit

from .. import config

_lock = threading.RLock()
DEFAULTS = {"base_url": "", "model": "gpt-oss-120b", "enabled": False}


def _path():
    return config.DATA_DIR / "text-api.json"


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


def load_settings():
    with _lock:
        try:
            value = json.loads(_path().read_text(encoding="utf-8"))
            return {**DEFAULTS, **value}
        except FileNotFoundError:
            return dict(DEFAULTS)


def api_key(settings=None):
    value = settings if settings is not None else load_settings()
    if value.get("key_dpapi"):
        return _crypt(base64.b64decode(value["key_dpapi"]), decrypt=True).decode("utf-8")
    return value.get("key_local", "")


def api_key_for(base_url):
    value = load_settings()
    if value["base_url"] != base_url:
        raise ValueError("文本 API 地址已变化，请重新发起请求")
    return api_key(value)


def public_settings():
    value = load_settings()
    return {**{k: value[k] for k in DEFAULTS}, "key_configured": bool(value.get("key_dpapi") or value.get("key_local"))}


def validate_url(value):
    if not isinstance(value, str) or len(value) > 500:
        raise ValueError("API 地址无效")
    value = value.strip().rstrip("/")
    url = urlsplit(value)
    if not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError("API 地址不能包含密钥、查询参数或账号密码")
    if url.scheme != "https" and not (url.scheme == "http" and url.hostname in {"localhost", "127.0.0.1", "::1"}):
        raise ValueError("远程 API 请使用 HTTPS，本机服务可使用 HTTP")
    return value


def save_settings(patch):
    if not isinstance(patch, dict) or set(patch) - {*DEFAULTS, "api_key"}:
        raise ValueError("文本 API 设置包含未知字段")
    with _lock:
        value = load_settings()
        if "base_url" in patch:
            value["base_url"] = validate_url(patch["base_url"])
        if "model" in patch:
            if not isinstance(patch["model"], str) or len(patch["model"]) > 200:
                raise ValueError("模型名称须为文本（最多 200 字符）")
            value["model"] = patch["model"].strip()
        if "enabled" in patch:
            if not isinstance(patch["enabled"], bool):
                raise ValueError("enabled 须为布尔值")
            value["enabled"] = patch["enabled"]
        # Blank leaves the existing credential in place, so reopening the form is safe.
        key = patch.get("api_key", "")
        if not isinstance(key, str) or len(key) > 2000 or any(c in key for c in "\r\n"):
            raise ValueError("API 密钥格式无效")
        key = key.strip()
        if key:
            value.pop("key_dpapi", None); value.pop("key_local", None)
            if os.name == "nt":
                value["key_dpapi"] = base64.b64encode(_crypt(key.encode())).decode()
            else:
                value["key_local"] = key
        if value["enabled"] and (not value["base_url"] or not value["model"]):
            raise ValueError("启用前请填写 API 地址和模型")
        path = _path(); path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(".tmp")
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False)
        temp.replace(path)
    return public_settings()
