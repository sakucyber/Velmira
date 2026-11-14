"""Local model pools, bounded failover and private 24-hour routing telemetry.

Only endpoint/model metadata is recorded. Prompts, responses and credentials never
enter this database. Scheduling is thread safe within a backend process.
"""
from __future__ import annotations

import copy
import json
import logging
import math
import random
import sqlite3
import threading
import time
import uuid

from .. import config
from . import connections, settings

log = logging.getLogger("velmira.routing")
PRESETS = {
    # Presets keep the common controls understandable while preserving the
    # existing low-level fields for advanced users.  Values are intentionally
    # conservative: a failed request can still fail over without making a
    # normal request wait several minutes.
    "stable": {"label": "稳健", "description": "更长超时、更多冷却时间，优先保证成功率。",
               "timeout": 180, "cooldown": 120, "temperature": 0.2, "max_tokens": 6000},
    "balanced": {"label": "平衡", "description": "在响应速度和成功率之间取平衡。",
                 "timeout": 120, "cooldown": 60, "temperature": 0.4, "max_tokens": 6000},
    "fast": {"label": "激进", "description": "更短等待时间，适合快速草稿和高吞吐。",
             "timeout": 60, "cooldown": 15, "temperature": 0.7, "max_tokens": 8000},
}
DEFAULTS = {"enabled": False, "strategy": "ordered", "models": [], "preset": "balanced", "timeout": 120,
            "cooldown": 60, "temperature": 0.2, "max_tokens": 6000, "revision": 0}
_lock = threading.RLock()
_cursors = {}
_cooldowns = {}


class RoutingConflict(ValueError):
    pass


class RoutingExhausted(RuntimeError):
    """All candidates failed; retain only safe status metadata for diagnostics."""
    def __init__(self, message, *, status_code=None, error_code=None, attempts=0):
        super().__init__(message)
        self.status_code = status_code
        self.error_code = error_code
        self.attempts = attempts


def _kind(kind):
    if kind not in ("text", "image"):
        raise ValueError("模型池类型须为 text 或 image")
    return kind


def _path(kind):
    return config.DATA_DIR / (_kind(kind) + "-routing.json")


def get_config(kind):
    with settings._lock:
        try:
            data = json.loads(_path(kind).read_text(encoding="utf-8"))
        except FileNotFoundError:
            data = {}
        return copy.deepcopy({**DEFAULTS, **data})


def is_enabled(kind):
    return get_config(kind)["enabled"]


def save_config(kind, patch):
    _kind(kind)
    if not isinstance(patch, dict) or set(patch) - set(DEFAULTS):
        raise ValueError("模型池设置包含未知字段")
    with settings._lock:
        old = get_config(kind)
        if "revision" in patch and (type(patch["revision"]) is not int or patch["revision"] != old["revision"]):
            raise RoutingConflict("模型池设置已变化，请刷新后再保存")
        # Selecting a preset updates its tunable defaults.  Explicit fields in
        # the same PATCH still win, so an advanced user can use a preset as a
        # starting point and fine tune one value without another save round.
        preset = patch.get("preset", old.get("preset", "balanced"))
        if not isinstance(preset, str) or preset not in PRESETS:
            raise ValueError("未知模型策略预设")
        selected_defaults = {k: v for k, v in PRESETS[preset].items() if k in DEFAULTS} if "preset" in patch else {}
        value = {**old, **selected_defaults, **patch, "preset": preset}
        if type(value["enabled"]) is not bool:
            raise ValueError("enabled 须为布尔值")
        if value["strategy"] not in ("ordered", "round_robin", "random"):
            raise ValueError("使用方式须为按顺序、轮流或随机")
        for field, low, high in (("timeout", 5, 600), ("cooldown", 0, 3600), ("max_tokens", 256, 32768)):
            if type(value[field]) is not int or not low <= value[field] <= high:
                raise ValueError(field + " 超出有效范围")
        temperature = value["temperature"]
        if isinstance(temperature, bool) or not isinstance(temperature, (int, float)) or not math.isfinite(temperature) or not 0 <= temperature <= 2:
            raise ValueError("温度须在 0–2 之间")
        models = value["models"]
        if not isinstance(models, list) or len(models) > 20 or value["enabled"] and not models:
            raise ValueError("启用前须添加 1–20 个模型")
        seen = set()
        for model in models:
            if not isinstance(model, dict) or set(model) != {"connection_id", "model"}:
                raise ValueError("每个候选须包含连接和模型")
            resolved = connections.resolve(model["connection_id"], model["model"], kind)
            key = (resolved["connection_id"], resolved["model"])
            if key in seen:
                raise ValueError("模型池不能重复添加同一连接的同一模型")
            seen.add(key)
        value["revision"] = old["revision"] + 1
        path = _path(kind)
        path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")
        temp.replace(path)
    return get_config(kind)


def fingerprint(kind):
    """Used in text caches so reordering or changing a pool invalidates old plans."""
    value = get_config(kind)
    if not value["enabled"]:
        return None
    return {"config": value, "connections": [connections.resolve(p["connection_id"], p["model"], kind) for p in value["models"]]}


def capacity(kind):
    value = get_config(kind)
    if not value["enabled"]:
        return 1
    endpoints = {}
    for item in value["models"]:
        cfg = connections.resolve(item["connection_id"], item["model"], kind)
        # Several models on one account share its concurrency budget.
        key = cfg["base_url"]
        endpoints[key] = min(endpoints.get(key, 8), cfg["max_concurrency"])
    return min(8, sum(endpoints.values()))


def _database():
    config.DATA_DIR.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(config.DATA_DIR / "model-routing.sqlite3", timeout=.25)
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("PRAGMA journal_mode=WAL")
        conn.executescript("""
        CREATE TABLE IF NOT EXISTS routing_requests (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at REAL NOT NULL,
            success INTEGER NOT NULL DEFAULT 0, fallback INTEGER NOT NULL DEFAULT 0, last_model TEXT);
        CREATE INDEX IF NOT EXISTS routing_requests_time ON routing_requests(kind,created_at);
        CREATE TABLE IF NOT EXISTS routing_attempts (
            id INTEGER PRIMARY KEY, request_id TEXT NOT NULL, kind TEXT NOT NULL,
            connection_id TEXT NOT NULL, model TEXT NOT NULL, created_at REAL NOT NULL,
            duration_ms REAL NOT NULL, success INTEGER NOT NULL, error_code TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS routing_attempts_time ON routing_attempts(kind,created_at);
        """)
    except BaseException:
        conn.close()
        raise
    return conn


def _record_request(kind, request_id, created_at, success, attempts, last_model):
    with _lock:
        conn = _database()
        try:
            with conn:
                conn.execute("INSERT INTO routing_requests VALUES(?,?,?,?,?,?)",
                             (request_id, kind, created_at, int(success), int(success and attempts > 1), last_model))
                conn.execute("DELETE FROM routing_attempts WHERE created_at<?", (time.time() - 86400,))
                conn.execute("DELETE FROM routing_requests WHERE created_at<?", (time.time() - 86400,))
        finally:
            conn.close()


def _record_attempt(kind, request_id, cfg, started, duration, success, error):
    with _lock:
        conn = _database()
        try:
            with conn:
                conn.execute("INSERT INTO routing_attempts(request_id,kind,connection_id,model,created_at,duration_ms,success,error_code) VALUES(?,?,?,?,?,?,?,?)",
                             (request_id, kind, cfg["connection_id"], cfg["model"], started, duration, int(success), error))
        finally:
            conn.close()


def _health_key(kind, cfg):
    return (str(config.DATA_DIR), kind, cfg["connection_id"], cfg["model"], cfg["base_url"])


def _safe_error(exc):
    code = getattr(exc, "status_code", None)
    if isinstance(code, int) and 100 <= code <= 599:
        return "HTTP " + str(code)
    if isinstance(exc, TimeoutError) or getattr(exc, "error_code", None) == "timeout":
        return "请求超时"
    if getattr(exc, "error_code", None) in ("invalid_response", "invalid_image", "empty_response"):
        return "返回内容无效"
    return "请求失败"


def _telemetry(fn, *args):
    # A failed statistics write must never turn a billed success into a retry.
    try:
        fn(*args)
    except (sqlite3.Error, OSError):
        log.warning("Model routing statistics unavailable")


def run(kind, invoke, *, fallback=None):
    value = get_config(kind)
    if not value["enabled"]:
        if fallback is None:
            raise RoutingExhausted("模型池未启用")
        return invoke(fallback)
    # Snapshot the public route metadata; API credentials are loaded per attempt.
    candidates = [{**connections.resolve(p["connection_id"], p["model"], kind),
                   **{k: value[k] for k in ("timeout", "temperature", "max_tokens")},
                   "_route_id": p["connection_id"] + ":" + p["model"]} for p in value["models"]]
    for cfg in candidates:
        shared = [c for c in candidates if c["base_url"] == cfg["base_url"]]
        cfg["max_concurrency"] = min(c["max_concurrency"] for c in shared)
        budgets = [c["rpm"] for c in shared if c["rpm"]]
        cfg["rpm"] = min(budgets) if budgets else 0
    with _lock:
        candidates = [c for c in candidates if not value["cooldown"] or _cooldowns.get(_health_key(kind, c), 0) <= time.time()]
        if value["strategy"] == "round_robin" and candidates:
            key = (str(config.DATA_DIR), kind, value["revision"])
            cursor = _cursors.get(key, 0)
            start = cursor % len(candidates)
            _cursors[key] = cursor + 1
            candidates = candidates[start:] + candidates[:start]
        elif value["strategy"] == "random":
            random.SystemRandom().shuffle(candidates)
    request_id, created_at = uuid.uuid4().hex, time.time()
    attempts, success, last_model = 0, False, None
    last_status, last_code = None, None
    deadline = time.monotonic() + value["timeout"]
    try:
        from .limits import RequestSlotBusy, wait_for_capacity
        while candidates:
            busy = []
            for cfg in candidates:
                health = _health_key(kind, cfg)
                with _lock:
                    if value["cooldown"] and _cooldowns.get(health, 0) > time.time():
                        continue
                started, timer = time.time(), time.monotonic()
                try:
                    result = invoke(cfg)
                except RequestSlotBusy:
                    # No network call has occurred, so try another connection;
                    # only busy candidates may be retried within this request.
                    busy.append(cfg)
                    continue
                except Exception as exc:
                    attempts += 1
                    last_model = cfg["model"]
                    # Keep status/category only; never retain upstream bodies.
                    from .diagnostics import provider_failure
                    raw_status = getattr(exc, "status_code", None)
                    last_status = raw_status if type(raw_status) is int and 100 <= raw_status <= 599 else None
                    last_code = provider_failure(exc)
                    with _lock:
                        _cooldowns[health] = time.time() + value["cooldown"]
                    _telemetry(_record_attempt, kind, request_id, cfg, started, (time.monotonic() - timer) * 1000, False, _safe_error(exc))
                    continue
                attempts += 1
                last_model = cfg["model"]
                with _lock:
                    _cooldowns.pop(health, None)
                _telemetry(_record_attempt, kind, request_id, cfg, started, (time.monotonic() - timer) * 1000, True, "")
                success = True
                return result
            if not busy:
                break
            if time.monotonic() >= deadline:
                raise RoutingExhausted("模型池连接繁忙，等待超时，请稍后重试", error_code="capacity", attempts=attempts)
            wait_for_capacity(min(.25, max(0, deadline - time.monotonic())))
            candidates = busy
        if not attempts:
            raise RoutingExhausted("模型池中的模型正在冷却，请稍后重试或调整模型池", error_code="cooldown", attempts=attempts)
        raise RoutingExhausted("模型池中的候选均请求失败，请检查模型连接或稍后重试",
                               status_code=last_status, error_code=last_code or "request_failed", attempts=attempts)
    finally:
        _telemetry(_record_request, kind, request_id, created_at, success, attempts, last_model)


def public(kind):
    value = get_config(kind)
    since = time.time() - 86400
    with _lock:
        conn = None
        try:
            conn = _database()
            requests = conn.execute("SELECT COUNT(*) AS requests,COALESCE(SUM(success),0) AS successes,COALESCE(SUM(fallback),0) AS fallbacks FROM routing_requests WHERE kind=? AND created_at>=?", (kind, since)).fetchone()
            rows = conn.execute("SELECT * FROM routing_attempts WHERE kind=? AND created_at>=? ORDER BY id", (kind, since)).fetchall()
            latest = conn.execute("SELECT last_model FROM routing_requests WHERE kind=? AND created_at>=? ORDER BY created_at DESC,rowid DESC LIMIT 1", (kind, since)).fetchone()
            available = True
        except (sqlite3.Error, OSError):
            requests = {"requests": 0, "successes": 0, "fallbacks": 0}
            rows, latest, available = [], None, False
        finally:
            if conn:
                conn.close()
        by_model = {}
        for row in rows:
            key = (row["connection_id"], row["model"])
            item = by_model.setdefault(key, {"attempts": 0, "successes": 0, "failures": 0, "total_ms": 0, "last_error": ""})
            item["attempts"] += 1
            item["successes"] += row["success"]
            item["failures"] += not row["success"]
            item["total_ms"] += row["duration_ms"]
            item["last_error"] = row["error_code"]
        models = []
        for model in value["models"]:
            cfg = connections.resolve(model["connection_id"], model["model"], kind)
            counts = by_model.get((model["connection_id"], model["model"]), {})
            attempts = counts.get("attempts", 0)
            models.append({**model, "label": cfg["label"], "attempts": attempts,
                           "successes": counts.get("successes", 0), "failures": counts.get("failures", 0),
                           "avg_ms": round(counts.get("total_ms", 0) / attempts, 1) if attempts else 0,
                           "last_error": counts.get("last_error", ""),
                           "cooldown_remaining": max(0, math.ceil(_cooldowns.get(_health_key(kind, cfg), 0) - time.time())) if value["cooldown"] else 0})
    status = {**dict(requests), "failures": sum(not r["success"] for r in rows),
              "request_failures": requests["requests"] - requests["successes"], "available": available,
              "attempts": len(rows), "last_model": latest[0] if latest else None, "models": models}
    return {"config": value, "presets": public_presets(kind), "status": status}


def public_presets(kind=None):
    """Return redacted preset metadata for the settings UI."""
    _kind(kind or "text") if kind else None
    return [{"id": key, "label": item["label"], "description": item["description"],
             "timeout": item["timeout"], "cooldown": item["cooldown"],
             "temperature": item["temperature"], "max_tokens": item["max_tokens"]}
            for key, item in PRESETS.items()]
