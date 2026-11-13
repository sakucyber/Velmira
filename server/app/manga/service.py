from __future__ import annotations

import io
import json
import time
import uuid

from PIL import Image

from .. import config, db
from ..ai.models import identity_hash, render_identity, stable_json, CARD_FIELDS
from .models import DEFAULTS, SHOTS, MangaError, build_prompt, check_identity, digest, resolve_form, validate_config


def uid(prefix):
    return prefix + uuid.uuid4().hex[:20]


def book(conn, book_id):
    if not conn.execute("SELECT 1 FROM books WHERE id=?", (book_id,)).fetchone():
        raise MangaError("书籍不存在", "book_not_found", 404)


def configuration(conn, book_id):
    book(conn, book_id)
    row = conn.execute("SELECT config_json FROM render_configs WHERE book_id=?", (book_id,)).fetchone()
    saved = json.loads(row[0]) if row else {}
    if saved and "style_preset" not in saved:
        # Migrate the previous built-in default to anime, preserving explicit custom styles.
        from .models import STYLE_PRESETS
        if saved.get("style") == STYLE_PRESETS["semi_realistic"]["prompt"]:
            saved["style"] = DEFAULTS["style"]
        else:
            saved["style_preset"] = "custom"
    return {**DEFAULTS, **saved}


def form_row(conn, book_id, form_id):
    row = conn.execute("SELECT f.* FROM character_forms f JOIN characters c ON c.id=f.character_id WHERE f.id=? AND c.book_id=?", (form_id, book_id)).fetchone()
    if row is None:
        raise MangaError("形态不存在", "form_not_found", 404)
    return row


def sync_base(conn, book_id):
    for char in conn.execute("SELECT * FROM characters WHERE book_id=?", (book_id,)).fetchall():
        form_id = "form_" + char["id"]
        existing = conn.execute("SELECT * FROM character_forms WHERE id=?", (form_id,)).fetchone()
        locked = bool(char["frozen"] and char["identity_hash"] and identity_hash(char["identity_block"] or "") == char["identity_hash"])
        if existing:
            if existing["identity_hash"] != (char["identity_hash"] or "") or existing["version"] != char["version"] or existing["locked"] != int(locked):
                stale(conn, form_id)
                conn.execute("UPDATE character_forms SET label=?,card_json=?,identity_block=?,identity_hash=?,version=?,locked=? WHERE id=?",
                             (char["name"] + " · 基础形态", char["card_json"], char["identity_block"] or "", char["identity_hash"] or "", char["version"], int(locked), form_id))
        else:
            conn.execute("INSERT OR IGNORE INTO character_forms(id,character_id,form_key,label,card_json,identity_block,identity_hash,version,locked) VALUES(?,?,?,?,?,?,?,?,?)",
                         (form_id, char["id"], "base", char["name"] + " · 基础形态", char["card_json"], char["identity_block"] or "", char["identity_hash"] or "", char["version"], int(locked)))


def stale(conn, form_id):
    conn.execute("UPDATE form_refs SET accepted=0 WHERE form_id=?", (form_id,))
    conn.execute("DELETE FROM auto_manga_refs WHERE ref_id IN (SELECT id FROM form_refs WHERE form_id=?)", (form_id,))
    conn.execute("UPDATE render_jobs SET status='stale',error='人物身份已变化，请按新版重新生成',updated_at=? WHERE form_id=? AND status!='cancelled'", (int(time.time()), form_id))


def asset_path(relative):
    target = (config.DATA_DIR / relative).resolve()
    if not target.is_relative_to(config.DATA_DIR.resolve()):
        raise MangaError("资产路径无效")
    return target


def save_image(raw, asset_id):
    if len(raw) > 16 * 1024 * 1024:
        raise MangaError("图片超过 16 MB")
    try:
        with Image.open(io.BytesIO(raw)) as image:
            if image.width > 4096 or image.height > 4096 or image.width < 32 or image.height < 32:
                raise MangaError("图片边长须为 32–4096 像素")
            image.load()
            relative = "assets/manga/" + asset_id + ".png"
            path = asset_path(relative); path.parent.mkdir(parents=True, exist_ok=True)
            image.convert("RGB").save(path, "PNG")
            return relative
    except (OSError, Image.DecompressionBombError) as exc:
        raise MangaError("图片格式无效") from exc


def public_ref(book_id, row):
    conn = db.get_conn()
    try:
        automatic = bool(conn.execute("SELECT 1 FROM auto_manga_refs WHERE ref_id=?", (row["id"],)).fetchone())
    finally:
        conn.close()
    return {key: row[key] for key in ["id", "form_id", "form_version", "identity_hash", "accepted", "created_at"]} | {"automatic": automatic, "url": f"/api/books/{book_id}/manga/assets/{row['id']}"}


def public_job(conn, row):
    result = dict(row); result.pop("snapshot_json")
    snapshot = json.loads(row["snapshot_json"])
    result["snapshot"] = snapshot
    result["url"] = f"/api/books/{row['book_id']}/manga/assets/{row['id']}" if row["image_path"] else None
    result.pop("image_path")
    result["history"] = []
    for attempt in conn.execute("SELECT * FROM render_attempts WHERE job_id=? ORDER BY idx", (row["id"],)).fetchall():
        item = dict(attempt); item["result"] = json.loads(item.pop("result_json"))
        item["workflow"] = json.loads(item.pop("workflow_json") or "null")
        item["url"] = f"/api/books/{row['book_id']}/manga/assets/{attempt['id']}" if item.pop("image_path") else None
        result["history"].append(item)
    return result


def enqueue(conn, book_id, payload, *, automatic=False):
    allowed = {"kind", "form_id", "character_id", "scene_id", "shot", "action", "seed", "flashback", "request_id", "para", "plan_id"}
    if not isinstance(payload, dict) or set(payload) - allowed:
        raise MangaError("出图请求包含未知字段")
    cfg = validate_config({}, configuration(conn, book_id))
    sync_base(conn, book_id)
    for key in ("form_id", "character_id", "scene_id", "plan_id"):
        if key in payload and (not isinstance(payload[key], str) or len(payload[key]) > 200):
            raise MangaError(key + " 须为字符串")
    kind = payload.get("kind", "panel")
    if not isinstance(kind, str) or kind not in {"panel", "reference"}:
        raise MangaError("kind 须为 panel 或 reference")
    shot = payload.get("shot", "portrait")
    action = payload.get("action", "")
    if not isinstance(action, str) or len(action) > 1200 or not isinstance(shot, str) or shot not in SHOTS:
        raise MangaError("镜头或画面描述无效")
    seed = payload.get("seed", cfg["seed"])
    if isinstance(seed, bool) or not isinstance(seed, int) or not 0 <= seed < 2**53:
        raise MangaError("seed 须为 0–2^53 的整数")
    flashback = payload.get("flashback", False)
    if not isinstance(flashback, bool):
        raise MangaError("flashback 须为布尔值")
    scene_row = None
    if kind == "panel":
        scene_row = conn.execute("SELECT * FROM scenes WHERE id=? AND book_id=?", (payload.get("scene_id"), book_id)).fetchone()
        if not scene_row:
            raise MangaError("请先选择本书的场景", "scene_not_found", 404)
        if scene_row["status"] == "needs_review":
            raise MangaError("该场景须先复核", "scene_needs_review", 409)
        scene = json.loads(scene_row["plan_json"])
        if len(scene.get("characters", [])) > 1 and not payload.get("plan_id"):
            raise MangaError("当前单参考模板每格支持一个人物，请先拆成单人镜头", "split_required", 409)
        chapter = scene_row["chapter_id"] or 0
        segment = conn.execute("SELECT start_para,end_para FROM segments WHERE id=? AND book_id=?", (scene_row["segment_id"], book_id)).fetchone()
        para = payload.get("para", segment[0] if segment else 0)
        if isinstance(para, bool) or not isinstance(para, int) or para < 0 or (segment and not segment[0] <= para <= segment[1]):
            raise MangaError("段落须位于所选场景的片段范围内")
        chosen = form_row(conn, book_id, payload["form_id"]) if payload.get("form_id") else None
        char_id = chosen["character_id"] if chosen else payload.get("character_id")
        if char_id not in scene.get("character_ids", []):
            raise MangaError("请选择该场景中的人物", "character_not_in_scene", 409)
        forms = [dict(r) for r in conn.execute("SELECT f.* FROM character_forms f JOIN characters c ON c.id=f.character_id WHERE c.book_id=? AND f.character_id=?", (book_id, char_id)).fetchall()]
        form = resolve_form(forms, chapter, para, payload.get("form_id"), flashback)
    else:
        if payload.get("plan_id"):
            raise MangaError("参考图任务不使用场景总结方案")
        form = dict(form_row(conn, book_id, payload.get("form_id")))
        check_identity(form)
        chapter, para = form["start_ch"], form["start_para"]
        scene = {"summary": "character reference portrait, one person, neutral expression, front view, head and shoulders, plain grey background"}
    ref = None
    if kind == "panel":
        ref_row = conn.execute("SELECT * FROM form_refs WHERE form_id=? AND accepted=1 AND form_version=? AND identity_hash=? ORDER BY created_at DESC,id LIMIT 1", (form["id"], form["version"], form["identity_hash"])).fetchone()
        if not ref_row and automatic:
            ref_row = conn.execute("SELECT r.* FROM form_refs r JOIN auto_manga_refs a ON a.ref_id=r.id WHERE a.book_id=? AND r.form_id=? AND r.form_version=? AND r.identity_hash=? ORDER BY r.created_at,r.id LIMIT 1",
                                   (book_id, form["id"], form["version"], form["identity_hash"])).fetchone()
        if not ref_row:
            raise MangaError("该形态尚无已确认且版本一致的参考图", "reference_required", 409)
        ref = dict(ref_row)
        if not asset_path(ref["path"]).is_file():
            raise MangaError("参考图文件不存在", "reference_missing", 409)
        ref["sha256"] = digest_bytes(asset_path(ref["path"]).read_bytes())
    elif automatic:
        # New outfits/ages keep the character's previous face as an editing reference.
        previous_ref = conn.execute("""SELECT r.* FROM form_refs r JOIN character_forms f ON f.id=r.form_id
            LEFT JOIN auto_manga_refs a ON a.ref_id=r.id
            WHERE f.character_id=? AND f.id!=? AND f.locked=1 AND f.start_ch<=?
            AND r.form_version=f.version AND r.identity_hash=f.identity_hash AND (r.accepted=1 OR a.book_id=?)
            ORDER BY f.start_ch DESC,f.start_para DESC,r.created_at DESC,r.id LIMIT 1""",
            (form["character_id"], form["id"], chapter, book_id)).fetchone()
        if previous_ref:
            ref = dict(previous_ref)
            ref["sha256"] = digest_bytes(asset_path(ref["path"]).read_bytes())
    text_plan = None
    if payload.get("plan_id"):
        from ..ai.director import resolve_plan
        text_plan = resolve_plan(conn, book_id, payload["plan_id"], {**payload, "form_id": form["id"]}, cfg)
    prompt = build_prompt(cfg, form["identity_block"], scene, shot, action, ref is not None,
                          visual_prompt=text_plan["visual_prompt"] if text_plan else None)
    snapshot = {"version": 1, "config": cfg, "form": dict(form), "reference": ref, "scene": scene,
                "scene_hash": digest(scene), "chapter": chapter, "para": para, "shot": shot, "action": action,
                "seed": seed, "prompt": prompt, "prompt_hash": identity_hash(prompt), "flashback": flashback}
    if text_plan:
        snapshot["text_plan"] = text_plan
    if automatic:
        snapshot["automatic_draft"] = True
    request_id = payload.get("request_id")
    if request_id is not None and (not isinstance(request_id, str) or not 1 <= len(request_id) <= 100):
        raise MangaError("request_id 须为 1–100 字符")
    job_id = "render_" + digest([book_id, request_id])[:24] if request_id else uid("render_")
    old = conn.execute("SELECT * FROM render_jobs WHERE id=?", (job_id,)).fetchone()
    if old:
        if old["config_hash"] != digest(snapshot):
            raise MangaError("同一 request_id 已用于不同的请求", "idempotency_conflict", 409)
        return old
    now = int(time.time())
    conn.execute("INSERT INTO render_jobs(id,book_id,kind,form_id,scene_id,snapshot_json,config_hash,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
                 (job_id, book_id, kind, form["id"], scene_row["id"] if scene_row else None, stable_json(snapshot), digest(snapshot), now, now))
    if kind == "panel":
        conn.execute("INSERT INTO panel_forms VALUES(?,?,?,?)", (job_id, form["id"], "explicit" if payload.get("form_id") else "timeline", int(flashback)))
    return conn.execute("SELECT * FROM render_jobs WHERE id=?", (job_id,)).fetchone()


def digest_bytes(raw):
    import hashlib
    return hashlib.sha256(raw).hexdigest()
