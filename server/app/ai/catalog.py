"""Model library metadata layered over legacy connection model-name arrays."""
from __future__ import annotations

import hashlib
import json
import math

from .. import config
from ..manga.models import MangaError
from . import connections, settings

CAPABILITIES = {"text", "image", "vision", "embedding", "tools"}
FIELDS = {"connection_id", "model", "label", "capabilities", "input_price", "output_price"}


def model_id(connection_id, model):
    return "model-" + hashlib.sha256((connection_id + "\0" + model).encode()).hexdigest()[:24]


def _metadata():
    path = config.DATA_DIR / "catalog.json"
    return json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}


def _save(metadata):
    path = config.DATA_DIR / "catalog.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(metadata, ensure_ascii=False, indent=2), encoding="utf-8")
    temp.replace(path)


def vendor(connection_id):
    item = next((c for c in connections.public() if c["id"] == connection_id), None)
    if not item:
        raise MangaError("厂商连接不存在", "vendor_not_found", 404)
    return item


def public():
    from .routing import get_config
    with settings._lock:
        vendors = connections.public()
        metadata = _metadata()
        pools = {kind: get_config(kind) for kind in ("text", "image")}
        models = []
        for item in vendors:
            for model in item["models"]:
                identity = model_id(item["id"], model)
                info = {"label": item["label"] + "/" + model, "capabilities": [item["kind"]], "input_price": None,
                        "output_price": None, **metadata.get(identity, {})}
                assigned = []
                for kind, pool in pools.items():
                    if {"connection_id": item["id"], "model": model} in pool["models"]:
                        assigned.append(("文本" if kind == "text" else "生图") + "模型池" + ("" if pool["enabled"] else "（已关闭）"))
                current = settings.public_settings(item["kind"])
                if current["enabled"] and current["base_url"] == item["base_url"] and current["model"] == model:
                    assigned.append(("文本" if item["kind"] == "text" else "生图") + "默认连接")
                models.append({"id": identity, "connection_id": item["id"], "model": model,
                               **info, "assignments": assigned})
        return {"vendors": vendors, "models": models}


def get_model(identity):
    item = next((m for m in public()["models"] if m["id"] == identity), None)
    if not item:
        raise MangaError("模型不存在", "catalog_model_not_found", 404)
    return item


def _validated(payload, old=None):
    if not isinstance(payload, dict) or set(payload) - FIELDS:
        raise ValueError("模型设置包含未知字段")
    label_supplied = "label" in payload
    value = {**({k: old[k] for k in FIELDS} if old else {}), **payload}
    item = vendor(value.get("connection_id"))
    model = value.get("model")
    if not isinstance(model, str) or not 1 <= len(model.strip()) <= 200 or any(ord(c) < 32 for c in model):
        raise ValueError("模型 ID 须为 1–200 字符")
    value["model"] = model.strip()
    # New models get a stable, useful display name without another form
    # interaction.  Existing explicit labels remain untouched on metadata
    # edits; an empty label is treated the same as an omitted one.
    label = value.get("label")
    generated_old = bool(old and isinstance(old.get("label"), str) and old["label"] in {
        old["model"], vendor(old["connection_id"])["label"] + "/" + old["model"]
    })
    if (not label_supplied and (label is None or generated_old)) or (isinstance(label, str) and not label.strip()):
        label = item["label"] + "/" + value["model"]
    if not isinstance(label, str) or not 1 <= len(label.strip()) <= 200:
        raise ValueError("模型显示名须为 1–200 字符")
    value["label"] = label.strip()
    caps = value.get("capabilities", [item["kind"]])
    if not isinstance(caps, list) or not caps or any(not isinstance(c, str) or c not in CAPABILITIES for c in caps):
        raise ValueError("请选择文本、生图、视觉、嵌入或工具能力")
    value["capabilities"] = list(dict.fromkeys(caps))
    # A model's capabilities must stay compatible with its connection kind:
    # adding a "tools only" model to a text vendor would silently make it
    # unusable in the text pool and hide the real routing mistake.
    if item["kind"] not in value["capabilities"]:
        raise ValueError("能力必须包含连接的" + ("文本" if item["kind"] == "text" else "生图") + "能力")
    for field in ("input_price", "output_price"):
        price = value.get(field)
        if price is not None and (isinstance(price, bool) or not isinstance(price, (int, float)) or not math.isfinite(price) or not 0 <= price <= 1e9):
            raise ValueError("价格须为空或非负数字")
        value[field] = price
    return value, item


def _unused(connection_id, model=None):
    from .routing import get_config
    for kind in ("text", "image"):
        if any(m["connection_id"] == connection_id and (model is None or m["model"] == model) for m in get_config(kind)["models"]):
            raise MangaError("请先从模型池移除此模型，再修改 ID 或删除", "model_in_use", 409)


def save_model(payload, identity=None):
    with settings._lock:
        old = get_model(identity) if identity else None
        value, item = _validated(payload, old)
        new_id = model_id(value["connection_id"], value["model"])
        if new_id != identity and value["model"] in item["models"]:
            raise MangaError("该厂商已存在相同模型 ID", "catalog_model_conflict", 409)
        if old and new_id != identity:
            _unused(old["connection_id"], old["model"])
        # Resolve and validate every field before changing either local file.
        metadata = _metadata()
        metadata[new_id] = {k: value[k] for k in ("label", "capabilities", "input_price", "output_price")}
        if old and new_id != identity:
            old_vendor = vendor(old["connection_id"])
            connections.save_connection({**{k: old_vendor[k] for k in ("id", "label", "kind", "models")},
                                         "models": [m for m in old_vendor["models"] if m != old["model"]]})
            metadata.pop(identity, None)
        if value["model"] not in item["models"]:
            # If this is an in-place rename, reload the shortened list.
            item = vendor(item["id"])
            connections.save_connection({**{k: item[k] for k in ("id", "label", "kind", "models")},
                                         "models": [*item["models"], value["model"]]})
        _save(metadata)
        return get_model(new_id)


def delete_model(identity):
    with settings._lock:
        old = get_model(identity)
        _unused(old["connection_id"], old["model"])
        item = vendor(old["connection_id"])
        connections.save_connection({**{k: item[k] for k in ("id", "label", "kind", "models")},
                                     "models": [m for m in item["models"] if m != old["model"]]})
        metadata = _metadata(); metadata.pop(identity, None); _save(metadata)


def delete_vendor(connection_id):
    with settings._lock:
        vendor(connection_id)
        _unused(connection_id)
        removed = {m["id"] for m in public()["models"] if m["connection_id"] == connection_id}
        items = [c for c in connections.entries() if c["id"] != connection_id]
        path = config.DATA_DIR / "connections.json"
        temp = path.with_suffix(".tmp")
        temp.write_text(json.dumps(items, ensure_ascii=False, indent=2), encoding="utf-8"); temp.replace(path)
        settings._path("profile-" + connection_id).unlink(missing_ok=True)
        _save({k: v for k, v in _metadata().items() if k not in removed})


def remote_models(connection_id):
    from .providers import request_json
    from .limits import request_slot
    item = vendor(connection_id)
    cfg = settings.load_settings("profile-" + connection_id)
    try:
        model_url = cfg["base_url"] if cfg["base_url"].rstrip("/").endswith("/models") else cfg["base_url"] + "/models"
        with request_slot((cfg["base_url"], item["kind"]), item["rpm"], item["max_concurrency"]):
            raw = request_json(model_url, api_key=settings.api_key(cfg), timeout=20)
        # OpenAI-compatible gateways normally return {"data": [{"id": …}]},
        # while several local servers return either a bare array or a
        # {"models": …} wrapper.  Normalize all three forms for the checkbox
        # importer.
        if isinstance(raw, dict):
            found = raw.get("data") or raw.get("models")
        else:
            found = raw
        if not isinstance(found, list):
            raise ValueError()
        result = []
        for model in found[:5000]:
            if isinstance(model, dict):
                name = model.get("id") or model.get("name") or model.get("model")
            else:
                name = model
            if isinstance(name, str) and 1 <= len(name) <= 200 and not any(ord(c) < 32 for c in name):
                result.append(name)
        return {"models": sorted(set(result))[:1000]}
    except Exception:
        raise MangaError("无法读取供应商模型列表，请检查地址与密钥；可手动添加模型", "catalog_models_failed", 502) from None


def probe(identity):
    model = get_model(identity)
    found = model["model"] in remote_models(model["connection_id"])["models"]
    return {"ok": found, "model": model["model"],
            "note": "供应商模型列表中可访问；未执行生成请求" if found else "供应商模型列表中未发现此模型；未执行生成请求"}


def add_to_pool(identity, kind=None):
    """Add a catalog model to its compatible routing pool.

    The catalog remains the source of the connection/model identity, while
    routing owns ordering and enablement.  This small operation lets clients
    offer an ``添加到模型池`` action without reconstructing a full routing
    payload (and without exposing credentials).
    """
    item = get_model(identity)
    selected = kind or item["kind"]
    if selected not in ("text", "image"):
        raise MangaError("模型只能加入文本或生图模型池", "pool_kind_invalid", 400)
    if selected != item["kind"]:
        raise MangaError("模型类型与目标模型池不匹配", "pool_kind_mismatch", 400)
    from . import routing
    config = routing.get_config(selected)
    member = {"connection_id": item["connection_id"], "model": item["model"]}
    if member not in config["models"]:
        if len(config["models"]) >= 20:
            raise MangaError("模型池最多添加 20 个模型", "pool_full", 400)
        routing.save_config(selected, {"models": [*config["models"], member]})
    return routing.public(selected)
