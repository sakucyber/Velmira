"""Editable long-term notes with explicit book/character scope and bounded retrieval."""
import json
import re
import time
import uuid

from .. import db
from ..manga.models import MangaError, digest
from . import features

KINDS = ("personality", "meme", "world", "note")


def _public(row):
    value = dict(row)
    value["tags"] = json.loads(value.pop("tags_json"))
    value["enabled"] = bool(value["enabled"])
    return value


def list_all(book_id=None, q=""):
    conn = db.get_conn()
    try:
        rows = conn.execute("SELECT * FROM ai_memories ORDER BY updated_at DESC,id").fetchall()
        memories = [_public(r) for r in rows if (not book_id or r["book_id"] == book_id) and
                    (not q or q.casefold() in (r["title"] + r["content"] + r["tags_json"]).casefold())]
        return {"memories": memories, "books": [dict(r) for r in conn.execute("SELECT id,title FROM books ORDER BY title")],
                "characters": [dict(r) for r in conn.execute("SELECT id,book_id,name FROM characters ORDER BY name")]}
    finally:
        conn.close()


def _validate(conn, value):
    for key, limit in (("title", 120), ("content", 10000)):
        if not isinstance(value[key], str) or not value[key].strip() or len(value[key]) > limit:
            raise MangaError(key + " 须为非空文本，且不超过 " + str(limit) + " 字符")
        value[key] = value[key].strip()
    if value["kind"] not in KINDS or type(value["enabled"]) is not bool:
        raise MangaError("记忆类型或启用状态无效")
    tags = value["tags"]
    if not isinstance(tags, list) or len(tags) > 20 or any(not isinstance(t, str) or not 1 <= len(t) <= 50 for t in tags):
        raise MangaError("标签最多20个，每个1–50字符")
    book_id, char_id = value["book_id"], value["character_id"]
    if book_id is not None and (not isinstance(book_id, str) or not conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone()):
        raise MangaError("记忆所属书籍不存在")
    if char_id is not None and (not isinstance(char_id, str) or not book_id or not conn.execute(
            "SELECT 1 FROM characters WHERE id=? AND book_id=?", (char_id, book_id)).fetchone()):
        raise MangaError("人物须属于所选书籍")
    return value


def save(patch, memory_id=None):
    allowed = {"title", "kind", "content", "tags", "book_id", "character_id", "enabled", "revision"}
    if not isinstance(patch, dict) or set(patch) - allowed:
        raise MangaError("记忆字段无效")
    conn = db.get_conn()
    try:
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            old = conn.execute("SELECT * FROM ai_memories WHERE id=?", (memory_id,)).fetchone() if memory_id else None
            if memory_id and not old:
                raise MangaError("记忆不存在", "memory_not_found", 404)
            if old and (type(patch.get("revision")) is not int or patch["revision"] != old["revision"]):
                raise MangaError("记忆已变化，请刷新", "revision_conflict", 409)
            value = _validate(conn, {**({"title": "", "content": "", "kind": "note", "tags": [], "book_id": None,
                                          "character_id": None, "enabled": True} if not old else _public(old)), **patch})
            identifier, now = memory_id or "memory_" + uuid.uuid4().hex, int(time.time())
            revision = old["revision"] + 1 if old else 1
            conn.execute("""INSERT INTO ai_memories(id,title,kind,content,tags_json,book_id,character_id,enabled,revision,updated_at)
                VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET title=excluded.title,kind=excluded.kind,
                content=excluded.content,tags_json=excluded.tags_json,book_id=excluded.book_id,character_id=excluded.character_id,
                enabled=excluded.enabled,revision=excluded.revision,updated_at=excluded.updated_at""",
                (identifier, value["title"], value["kind"], value["content"], json.dumps(value["tags"], ensure_ascii=False),
                 value["book_id"], value["character_id"], int(value["enabled"]), revision, now))
            return _public(conn.execute("SELECT * FROM ai_memories WHERE id=?", (identifier,)).fetchone())
    finally:
        conn.close()


def delete(memory_id, revision):
    conn = db.get_conn()
    try:
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            row = conn.execute("SELECT revision FROM ai_memories WHERE id=?", (memory_id,)).fetchone()
            if not row:
                raise MangaError("记忆不存在", "memory_not_found", 404)
            if type(revision) is not int or row[0] != revision:
                raise MangaError("记忆已变化，请刷新", "revision_conflict", 409)
            conn.execute("DELETE FROM ai_memories WHERE id=? AND revision=?", (memory_id, revision))
    finally:
        conn.close()


def retrieve(conn, book_id, character_ids, text):
    cfg = features.load()["memory"]
    if not cfg["enabled"]:
        return []
    ids = set(character_ids)
    terms = set(re.findall(r"[a-zA-Z0-9_]{2,30}|[\u4e00-\u9fff]{2}", text.casefold()[:12000]))
    matches = []
    for row in conn.execute("SELECT * FROM ai_memories WHERE enabled=1 AND (book_id IS NULL OR book_id=?)", (book_id,)):
        if row["character_id"] and row["character_id"] not in ids:
            continue
        value = _public(row)
        content = (value["title"] + " " + value["content"] + " " + " ".join(value["tags"])).casefold()
        score = (20 if value["character_id"] else 6 if value["book_id"] else 0) + sum(t in content for t in terms)
        # Explicit book/character notes are relevant even if their wording differs.
        if not score:
            continue
        matches.append((score, value))
    matches.sort(key=lambda pair: (-pair[0], pair[1]["id"]))
    selected, left = [], cfg["max_chars"]
    for _, value in matches[:cfg["max_items"]]:
        if left <= 0:
            break
        value["content"] = value["content"][:left]
        left -= len(value["content"])
        selected.append({k: value[k] for k in ("id", "title", "kind", "content", "tags", "revision")})
    return selected


def fingerprint():
    conn = db.get_conn()
    try:
        return digest([dict(r) for r in conn.execute("SELECT id,revision,enabled FROM ai_memories ORDER BY id")])
    finally:
        conn.close()
