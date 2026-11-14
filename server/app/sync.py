"""Local-first synchronization primitives.

The service is deliberately transport agnostic: clients keep their own local
store and send immutable operations when connected. This module validates and
applies the small set of user data entities that can be merged safely.
"""
from __future__ import annotations

import json
import secrets
import time

from . import db
from .api.books import _compute_percent

MAX_OPERATIONS = 200
MAX_PAYLOAD_BYTES = 64 * 1024
ENTITIES = {"progress", "bookmark", "annotation", "memory"}
ACTIONS = {"upsert", "delete"}
_FIELDS = {
    "progress": {"chapter", "para", "offset", "percent", "updated_at"},
    "bookmark": {"book_id", "chapter", "para", "offset", "label", "created_at"},
    "annotation": {"book_id", "chapter", "para", "start_offset", "end_offset", "text", "note", "color", "created_at", "updated_at"},
    "memory": {"title", "kind", "content", "tags", "book_id", "character_id", "enabled", "revision", "updated_at"},
}


def _location(conn, book_id, chapter, para=0, offset=0):
    book = conn.execute("SELECT format,chapter_count FROM books WHERE id=?", (book_id,)).fetchone()
    if not book:
        raise SyncError("同步对应的书籍不存在", "book_not_found", 404)
    _integer(chapter, "chapter"); _integer(para, "para"); _integer(offset, "offset")
    if chapter >= book["chapter_count"]:
        raise SyncError("章节越界")
    if book["format"] == "cbz":
        if para != 0 or offset != 0: raise SyncError("漫画进度位置无效")
        return None
    row = conn.execute("SELECT paras_json FROM chapters WHERE book_id=? AND idx=?", (book_id, chapter)).fetchone()
    paras = json.loads(row[0]) if row else []
    if para >= len(paras) or offset > len(paras[para]):
        raise SyncError("段落位置越界")
    return paras[para]


def _revision(conn, entity, entity_id):
    row = conn.execute("SELECT revision FROM sync_entity_states WHERE entity=? AND entity_id=?", (entity, entity_id)).fetchone()
    return int(row[0] or 0) if row else 0


def _remember(conn, entity, entity_id, payload, revision):
    encoded = None if payload is None else _json_payload(payload)
    conn.execute("INSERT INTO sync_entity_states(entity,entity_id,payload_json,revision) VALUES(?,?,?,?) ON CONFLICT(entity,entity_id) DO UPDATE SET payload_json=excluded.payload_json,revision=excluded.revision", (entity, entity_id, encoded, revision))


def _current_entities(conn):
    values = {}
    for entity, table, key in (("progress", "progress", "book_id"), ("bookmark", "bookmarks", "id"), ("annotation", "annotations", "id")):
        for row in conn.execute("SELECT * FROM " + table):
            item = dict(row); entity_id = item.pop(key)
            values[(entity, entity_id)] = item
    for row in conn.execute("SELECT * FROM ai_memories"):
        item = dict(row); entity_id = item.pop("id")
        item["tags"] = json.loads(item.pop("tags_json"))
        item["enabled"] = bool(item["enabled"])
        values[("memory", entity_id)] = item
    return values


def _capture_local_changes(conn):
    """Capture existing REST mutations as operations before serving a sync turn.

    This keeps the old online reader and new offline clients consistent without
    coupling every API route to the sync transport. No model configuration or
    credential table is included in this explicit projection.
    """
    now = int(time.time())
    conn.execute("INSERT OR IGNORE INTO sync_devices(id,label,created_at,last_seen) VALUES('server','本机服务',?,?)", (now, now))
    values = _current_entities(conn)
    states = {(r["entity"], r["entity_id"]): r["payload_json"] for r in conn.execute("SELECT * FROM sync_entity_states")}
    sequence = conn.execute("SELECT COALESCE(MAX(seq),0) FROM sync_operations WHERE device_id='server'").fetchone()[0]
    for identity in sorted(set(values) | set(states)):
        entity, entity_id = identity
        payload = values.get(identity)
        encoded = None if payload is None else _json_payload(payload)
        if encoded == states.get(identity):
            continue
        sequence += 1
        revision = _revision(conn, entity, entity_id)
        cursor = conn.execute("INSERT INTO sync_operations(op_id,device_id,seq,entity,entity_id,action,payload_json,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?,?)", ("server_" + secrets.token_urlsafe(18), "server", sequence, entity, entity_id, "delete" if payload is None else "upsert", _json_payload(payload or {}), revision, now)).lastrowid
        _remember(conn, entity, entity_id, payload, cursor)


def _validate_fields(operation, entity, payload):
    if set(operation) - {"op_id", "seq", "entity", "entity_id", "action", "payload", "base_revision"}:
        raise SyncError("操作含未知字段")
    if set(payload) - _FIELDS[entity]:
        raise SyncError("payload 含未知字段")
    if "base_revision" in operation:
        _integer(operation["base_revision"], "base_revision")


class SyncError(ValueError):
    def __init__(self, message: str, code: str = "sync_invalid", status: int = 400):
        super().__init__(message)
        self.code, self.status = code, status


def _text(value, name, limit=200):
    if not isinstance(value, str) or not 1 <= len(value) <= limit:
        raise SyncError(f"{name} 格式无效")
    return value


def _integer(value, name, minimum=0, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum or (maximum is not None and value > maximum):
        raise SyncError(f"{name} 格式无效")
    return value


def register_device(payload, session_hash=None, active_session_hashes=None):
    if not isinstance(payload, dict):
        raise SyncError("设备请求必须是 JSON 对象")
    if set(payload) - {"device_id", "label"}:
        raise SyncError("设备请求含未知字段")
    device_id = payload.get("device_id") or "dev_" + secrets.token_urlsafe(18)
    _text(device_id, "device_id", 100)
    if device_id == "server": raise SyncError("设备标识已保留")
    label = payload.get("label", "")
    if not isinstance(label, str) or len(label) > 120:
        raise SyncError("设备名称过长")
    now = int(time.time())
    conn = db.get_conn()
    try:
        with conn:
            row = conn.execute("SELECT id,created_at,revoked_at FROM sync_devices WHERE id=?", (device_id,)).fetchone()
            if row and row["revoked_at"]:
                raise SyncError("设备已撤销，请重新注册", "device_revoked", 409)
            if session_hash is not None and row:
                owner = conn.execute("SELECT session_hash FROM sync_device_sessions WHERE device_id=?", (device_id,)).fetchone()
                if not owner:
                    raise SyncError("此设备标识属于其他会话", "device_session_mismatch", 403)
                if owner[0] != session_hash:
                    if active_session_hashes is None or owner[0] in active_session_hashes:
                        raise SyncError("此设备标识属于其他会话", "device_session_mismatch", 403)
                    # A user who signs out and pairs again may keep this local
                    # store. A live old session cannot be taken over; a revoked
                    # device is rejected above even when its owner has expired.
                    conn.execute("UPDATE sync_device_sessions SET session_hash=? WHERE device_id=?", (session_hash, device_id))
            if row:
                conn.execute("UPDATE sync_devices SET label=?,last_seen=? WHERE id=?", (label, now, device_id))
                created = row["created_at"]
            else:
                conn.execute("INSERT INTO sync_devices(id,label,created_at,last_seen) VALUES(?,?,?,?)", (device_id, label, now, now))
                created = now
                if session_hash is not None:
                    conn.execute("INSERT INTO sync_device_sessions(device_id,session_hash) VALUES(?,?)", (device_id, session_hash))
            last_seq = conn.execute("SELECT COALESCE(MAX(seq),0) FROM sync_operations WHERE device_id=?", (device_id,)).fetchone()[0]
    finally:
        conn.close()
    return {"device": {"id": device_id, "label": label, "created_at": created, "last_seen": now, "last_seq": last_seq}}


def check_device_session(device_id, session_hash):
    """A paired browser may use only its own device identifier."""
    _text(device_id, "device_id", 100)
    if session_hash is None:
        return
    conn = db.get_conn()
    try:
        owner = conn.execute("SELECT session_hash FROM sync_device_sessions WHERE device_id=?", (device_id,)).fetchone()
    finally: conn.close()
    if not owner or owner[0] != session_hash:
        raise SyncError("同步设备不属于当前配对会话", "device_session_mismatch", 403)


def _ensure_device(conn, device_id):
    row = conn.execute("SELECT * FROM sync_devices WHERE id=?", (device_id,)).fetchone()
    if not row:
        raise SyncError("设备未注册", "device_required", 409)
    if row["revoked_at"]:
        raise SyncError("设备已撤销，请重置同步身份后重新配对", "device_revoked", 409)
    return row


def _json_payload(payload):
    try:
        encoded = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        raise SyncError("操作数据不可序列化") from None
    if len(encoded.encode("utf-8")) > MAX_PAYLOAD_BYTES:
        raise SyncError("单条同步操作过大")
    return encoded


def _book_exists(conn, book_id):
    return conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone() is not None


def _apply_progress(conn, entity_id, action, payload):
    if action == "delete":
        raise SyncError("阅读进度不支持删除")
    if not _book_exists(conn, entity_id):
        raise SyncError("进度对应的书籍不存在", "book_not_found", 404)
    for key in ("chapter", "para", "offset"):
        _integer(payload.get(key, 0), key, 0)
    _location(conn, entity_id, payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0))
    percent = payload.get("percent", 0)
    if isinstance(percent, bool) or not isinstance(percent, (int, float)) or not 0 <= percent <= 1:
        raise SyncError("percent 格式无效")
    # Percent is derived from the validated source location, just like the
    # regular reader API. A stale/offline client cannot change this formula.
    percent = _compute_percent(conn, entity_id, payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0))
    updated = _integer(payload.get("updated_at", int(time.time())), "updated_at", 0)
    old = conn.execute("SELECT updated_at FROM progress WHERE book_id=?", (entity_id,)).fetchone()
    if old and int(old[0] or 0) > updated:
        return False
    conn.execute("INSERT INTO progress(book_id,chapter,para,offset,percent,updated_at) VALUES(?,?,?,?,?,?) "
                 "ON CONFLICT(book_id) DO UPDATE SET chapter=excluded.chapter,para=excluded.para,offset=excluded.offset,percent=excluded.percent,updated_at=excluded.updated_at",
                 (entity_id, payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0), percent, updated))
    return True


def _apply_bookmark(conn, entity_id, action, payload):
    if action == "delete":
        conn.execute("DELETE FROM bookmarks WHERE id=?", (entity_id,))
        return True
    book_id = _text(payload.get("book_id"), "book_id")
    if not _book_exists(conn, book_id):
        raise SyncError("书签对应的书籍不存在", "book_not_found", 404)
    for key in ("chapter", "para", "offset", "created_at"):
        _integer(payload.get(key, 0), key, 0)
    _location(conn, book_id, payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0))
    label = payload.get("label")
    if label is not None and (not isinstance(label, str) or len(label) > 500):
        raise SyncError("书签标签过长")
    duplicate = conn.execute("SELECT id FROM bookmarks WHERE book_id=? AND chapter=? AND para=?", (book_id, payload.get("chapter", 0), payload.get("para", 0))).fetchone()
    if duplicate and duplicate[0] != entity_id:
        return {"reason": "duplicate_bookmark", "canonical_id": duplicate[0]}
    existing = conn.execute("SELECT book_id,chapter,para FROM bookmarks WHERE id=?", (entity_id,)).fetchone()
    if existing and (existing["book_id"], existing["chapter"], existing["para"]) != (book_id, payload.get("chapter", 0), payload.get("para", 0)):
        raise SyncError("修改书签不能改变原文定位")
    conn.execute("INSERT OR IGNORE INTO bookmarks(id,book_id,chapter,para,offset,label,created_at) VALUES(?,?,?,?,?,?,?)",
                 (entity_id, book_id, payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0), label, payload.get("created_at", int(time.time()))))
    return True


def _apply_annotation(conn, entity_id, action, payload):
    if action == "delete":
        conn.execute("DELETE FROM annotations WHERE id=?", (entity_id,))
        return True
    book_id = _text(payload.get("book_id"), "book_id")
    if not _book_exists(conn, book_id):
        raise SyncError("批注对应的书籍不存在", "book_not_found", 404)
    required = ("chapter", "para", "start_offset", "end_offset", "text")
    for key in required[:-1]:
        _integer(payload.get(key), key, 0)
    text = payload.get("text")
    if not isinstance(text, str) or len(text) != payload["end_offset"] - payload["start_offset"]:
        raise SyncError("批注文本与偏移不一致")
    paragraph = _location(conn, book_id, payload["chapter"], payload["para"], payload["end_offset"])
    if paragraph is None or payload["end_offset"] <= payload["start_offset"] or paragraph[payload["start_offset"]:payload["end_offset"]] != text:
        raise SyncError("批注文本不匹配原文")
    if payload.get("color", "yellow") not in {"yellow", "green", "blue", "pink"}:
        raise SyncError("批注颜色无效")
    if payload.get("note") is not None and (not isinstance(payload["note"], str) or len(payload["note"]) > 20000):
        raise SyncError("批注内容无效")
    now = _integer(payload.get("updated_at", int(time.time())), "updated_at", 0)
    old = conn.execute("SELECT * FROM annotations WHERE id=?", (entity_id,)).fetchone()
    if old and any(old[key] != payload[key] for key in ("book_id", "chapter", "para", "start_offset", "end_offset", "text")):
        raise SyncError("修改批注不能改变原文定位")
    if old and int(old["updated_at"] or 0) > now:
        return False
    conn.execute("INSERT INTO annotations(id,book_id,chapter,para,start_offset,end_offset,text,note,color,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?) "
                 "ON CONFLICT(id) DO UPDATE SET note=excluded.note,color=excluded.color,updated_at=excluded.updated_at",
                 (entity_id, book_id, payload["chapter"], payload["para"], payload["start_offset"], payload["end_offset"], text, payload.get("note"), payload.get("color", "yellow"), payload.get("created_at", now), now))
    return True


def _apply_memory(conn, entity_id, action, payload):
    if action == "delete":
        conn.execute("DELETE FROM ai_memories WHERE id=?", (entity_id,))
        return True
    title, content = _text(payload.get("title"), "title", 120), _text(payload.get("content"), "content", 10000)
    tags = payload.get("tags", [])
    if not isinstance(tags, list) or len(tags) > 20 or any(not isinstance(x, str) or not 1 <= len(x) <= 50 for x in tags):
        raise SyncError("记忆标签格式无效")
    if payload.get("kind", "note") not in {"personality", "meme", "world", "note"} or type(payload.get("enabled", True)) is not bool:
        raise SyncError("记忆类型或启用状态无效")
    if payload.get("book_id") is not None and not _book_exists(conn, _text(payload["book_id"], "book_id")):
        raise SyncError("记忆对应的书籍不存在", "book_not_found", 404)
    if payload.get("character_id") is not None:
        character = conn.execute("SELECT book_id FROM characters WHERE id=?", (_text(payload["character_id"], "character_id"),)).fetchone()
        if not character or character[0] != payload.get("book_id"):
            raise SyncError("记忆人物不属于所选书籍")
    _integer(payload.get("revision", 1), "revision", 1)
    now = _integer(payload.get("updated_at", int(time.time())), "updated_at", 0)
    conn.execute("INSERT INTO ai_memories(id,title,kind,content,tags_json,book_id,character_id,enabled,revision,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?) "
                 "ON CONFLICT(id) DO UPDATE SET title=excluded.title,kind=excluded.kind,content=excluded.content,tags_json=excluded.tags_json,book_id=excluded.book_id,character_id=excluded.character_id,enabled=excluded.enabled,revision=ai_memories.revision+1,updated_at=excluded.updated_at",
                 (entity_id, title, payload.get("kind", "note"), content, json.dumps(tags, ensure_ascii=False), payload.get("book_id"), payload.get("character_id"), int(bool(payload.get("enabled", True))), int(payload.get("revision", 1)), now))
    return True


def apply_operation(conn, operation):
    entity = _text(operation.get("entity"), "entity", 30)
    if entity not in ENTITIES:
        raise SyncError("不支持的同步实体")
    action = _text(operation.get("action"), "action", 20)
    if action not in ACTIONS:
        raise SyncError("不支持的同步操作")
    entity_id = _text(operation.get("entity_id"), "entity_id", 200)
    payload = operation.get("payload", {})
    if not isinstance(payload, dict):
        raise SyncError("payload 必须是对象")
    _validate_fields(operation, entity, payload)
    if entity == "progress": return _apply_progress(conn, entity_id, action, payload)
    if entity == "bookmark": return _apply_bookmark(conn, entity_id, action, payload)
    if entity == "annotation": return _apply_annotation(conn, entity_id, action, payload)
    if entity == "memory": return _apply_memory(conn, entity_id, action, payload)


def push(device_id, operations):
    _text(device_id, "device_id", 100)
    if not isinstance(operations, list) or not 1 <= len(operations) <= MAX_OPERATIONS:
        raise SyncError(f"operations 数量须为1–{MAX_OPERATIONS}")
    conn = db.get_conn(); accepted = []; duplicates = []; conflicts = []
    try:
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            _ensure_device(conn, device_id)
            _capture_local_changes(conn)
            now = int(time.time())
            for raw in operations:
                if not isinstance(raw, dict): raise SyncError("操作必须是对象")
                op_id, seq = _text(raw.get("op_id"), "op_id", 120), _integer(raw.get("seq"), "seq", 1)
                existing = conn.execute("SELECT cursor,device_id,seq,entity,entity_id,action,payload_json FROM sync_operations WHERE op_id=?", (op_id,)).fetchone()
                if existing:
                    if existing["device_id"] != device_id or existing["seq"] != seq or any(existing[key] != raw.get(key) for key in ("entity", "entity_id", "action")) or json.loads(existing["payload_json"]) != raw.get("payload", {}):
                        raise SyncError("操作标识已被其他操作使用", "operation_conflict", 409)
                    duplicates.append(op_id); continue
                if conn.execute("SELECT 1 FROM sync_operations WHERE device_id=? AND seq=?", (device_id, seq)).fetchone():
                    raise SyncError("设备序号重复但操作标识不同", "sequence_conflict", 409)
                entity = _text(raw.get("entity"), "entity", 30)
                if entity not in ENTITIES or raw.get("action") not in ACTIONS or not isinstance(raw.get("payload", {}), dict):
                    raise SyncError("操作类型或数据格式无效")
                _text(raw.get("entity_id"), "entity_id", 200)
                _validate_fields(raw, entity, raw.get("payload", {}))
                _json_payload(raw.get("payload", {}))
                # An edit based on an older revision cannot silently overwrite
                # another device's annotation or memory. The client retains its
                # local draft and can ask the user to resolve the conflict.
                current_revision = _revision(conn, raw.get("entity"), raw.get("entity_id"))
                state = conn.execute("SELECT payload_json FROM sync_entity_states WHERE entity=? AND entity_id=?", (raw.get("entity"), raw.get("entity_id"))).fetchone()
                current_payload = json.loads(state[0]) if state and state[0] is not None else {}
                # A device clock may be ahead of this client. Explicitly
                # keeping the local draft must be able to supersede that time.
                conflict_time = {"current_updated_at": current_payload.get("updated_at", 0)} if entity in {"progress", "annotation"} else {}
                if raw.get("entity") in {"annotation", "memory"} and current_revision != raw.get("base_revision", 0):
                    conflicts.append({"op_id": op_id, "entity": raw.get("entity"), "entity_id": raw.get("entity_id"), "reason": "revision_conflict", "revision": current_revision, **conflict_time})
                    continue
                if raw.get("entity") == "bookmark" and raw.get("action") == "upsert" and state and state[0] is None and current_revision > raw.get("base_revision", 0):
                    conflicts.append({"op_id": op_id, "entity": "bookmark", "entity_id": raw.get("entity_id"), "reason": "deleted", "revision": current_revision})
                    continue
                applied = apply_operation(conn, raw)
                if isinstance(applied, dict):
                    conflicts.append({"op_id": op_id, "entity": raw["entity"], "entity_id": raw["entity_id"], "revision": current_revision, **applied})
                    continue
                if applied is False:
                    conflicts.append({"op_id": op_id, "entity": raw["entity"], "entity_id": raw["entity_id"], "reason": "server_newer", "revision": current_revision, **conflict_time})
                    continue
                payload_json = _json_payload(raw.get("payload", {}))
                cur = conn.execute("INSERT INTO sync_operations(op_id,device_id,seq,entity,entity_id,action,payload_json,base_revision,created_at) VALUES(?,?,?,?,?,?,?,?,?)",
                                   (op_id, device_id, seq, raw["entity"], raw["entity_id"], raw["action"], payload_json, raw.get("base_revision"), now)).lastrowid
                accepted.append({"op_id": op_id, "cursor": cur})
                canonical = _current_entities(conn).get((raw["entity"], raw["entity_id"]))
                _remember(conn, raw["entity"], raw["entity_id"], None if raw["action"] == "delete" else canonical, cur)
            conn.execute("UPDATE sync_devices SET last_seen=? WHERE id=?", (now, device_id))
    finally: conn.close()
    return {"accepted": accepted, "duplicates": duplicates, "conflicts": conflicts}


def pull(device_id, cursor=0, limit=100, include_memory=False):
    _text(device_id, "device_id", 100); cursor = _integer(cursor, "cursor", 0); limit = _integer(limit, "limit", 1, 200)
    conn = db.get_conn()
    try:
        conn.execute("BEGIN IMMEDIATE")
        _ensure_device(conn, device_id)
        _capture_local_changes(conn)
        rows = conn.execute("SELECT cursor,op_id,device_id,seq,entity,entity_id,action,payload_json,base_revision,created_at FROM sync_operations WHERE cursor>? ORDER BY cursor LIMIT ?", (cursor, limit + 1)).fetchall()
        more = len(rows) > limit; rows = rows[:limit]
        operations = [{"cursor": r["cursor"], "op_id": r["op_id"], "device_id": r["device_id"], "seq": r["seq"], "entity": r["entity"], "entity_id": r["entity_id"], "action": r["action"], "payload": json.loads(r["payload_json"]), "base_revision": r["base_revision"], "created_at": r["created_at"]} for r in rows if include_memory or r["entity"] != "memory"]
        # Keep the submitted payload in the journal for strict idempotency,
        # while publishing the same derived value as snapshots and online GET.
        for operation in operations:
            if operation["entity"] == "progress" and operation["action"] == "upsert":
                payload = operation["payload"]
                payload["percent"] = _compute_percent(conn, operation["entity_id"], payload.get("chapter", 0), payload.get("para", 0), payload.get("offset", 0))
        next_cursor = int(rows[-1]["cursor"]) if rows else cursor
        conn.execute("UPDATE sync_devices SET last_seen=? WHERE id=?", (int(time.time()), device_id)); conn.commit()
    finally: conn.close()
    return {"operations": operations, "cursor": next_cursor, "has_more": more}


def snapshot(book_id=None, offset=0, limit=500):
    offset = _integer(offset, "offset", 0)
    limit = _integer(limit, "limit", 1, 1000)
    conn = db.get_conn()
    try:
        conn.execute("BEGIN IMMEDIATE")
        _capture_local_changes(conn)
        args = [] if book_id is None else [book_id]
        where = "" if book_id is None else " WHERE id=?"
        books = [dict(row) for row in conn.execute("SELECT id,title,author,format,sha256,total_chars,chapter_count,created_at FROM books" + where + " ORDER BY id LIMIT ? OFFSET ?", [*args, limit + 1, offset]).fetchall()]
        result = {"books": books, "progress": [], "bookmarks": [], "annotations": []}
        for name, key in (("progress", "book_id"), ("bookmarks", "id"), ("annotations", "id")):
            clause = " WHERE book_id=?" if book_id is not None else " WHERE book_id IN (SELECT id FROM books)"
            result[name] = [dict(r) for r in conn.execute("SELECT * FROM " + name + clause + " ORDER BY " + key + " LIMIT ? OFFSET ?", [*args, limit + 1, offset]).fetchall()]
        more = any(len(result[name]) > limit for name in ("books", "progress", "bookmarks", "annotations"))
        for name in ("books", "progress", "bookmarks", "annotations"): result[name] = result[name][:limit]
        for entity, name, key in (("progress", "progress", "book_id"), ("bookmark", "bookmarks", "id"), ("annotation", "annotations", "id")):
            for item in result[name]:
                item["sync_revision"] = _revision(conn, entity, item[key])
        result["cursor"] = int(conn.execute("SELECT COALESCE(MAX(cursor),0) FROM sync_operations").fetchone()[0])
        result.update(offset=offset, next_offset=offset + limit, has_more=more)
        conn.commit()
    finally: conn.close()
    return result
