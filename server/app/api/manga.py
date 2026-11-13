"""M3: reviewed reference assets and a persistent, serial single-panel queue."""
from __future__ import annotations

import json
import time
from contextlib import contextmanager

from fastapi import APIRouter, Body, Query, Request
from fastapi.responses import FileResponse

from .. import db
from ..ai.models import CARD_FIELDS, identity_hash, render_identity, stable_json
from ..manga import service as s
from ..manga.models import MangaError, check_identity, validate_config
from ..manga.provider import ComfyProvider
from ..manga.queue import valid_snapshot

router = APIRouter(prefix="/api/books/{book_id}/manga")


@contextmanager
def connection(book_id):
    conn = db.get_conn()
    try:
        s.book(conn, book_id)
        s.sync_base(conn, book_id)
        yield conn
        conn.commit()
    except BaseException:
        conn.rollback()
        raise
    finally:
        conn.close()


def fields(payload, allowed):
    if not isinstance(payload, dict) or set(payload) - set(allowed):
        raise MangaError("请求包含未知字段或格式无效")


def get_job(conn, book_id, job_id):
    row = conn.execute("SELECT * FROM render_jobs WHERE id=? AND book_id=?", (job_id, book_id)).fetchone()
    if not row:
        raise MangaError("生成任务不存在", "job_not_found", 404)
    return row


@router.get("/config")
def get_config(book_id: str):
    with connection(book_id) as conn:
        return {"config": s.configuration(conn, book_id)}


@router.patch("/config")
def patch_config(book_id: str, payload: dict = Body(...)):
    with connection(book_id) as conn:
        cfg = validate_config(payload, s.configuration(conn, book_id))
        conn.execute("INSERT INTO render_configs VALUES(?,?) ON CONFLICT(book_id) DO UPDATE SET config_json=excluded.config_json", (book_id, stable_json(cfg)))
        return {"config": cfg}


@router.post("/probe")
def probe(book_id: str):
    with connection(book_id) as conn:
        cfg = s.configuration(conn, book_id)
    if cfg["provider"] == "mock":
        return {"ok": True, "provider": "mock", "reference_mode": "mock", "missing": [], "note": "占位图仅供流程验证，不代表人物一致性通过"}
    return ComfyProvider(cfg).probe()


@router.get("/forms")
def forms(book_id: str):
    with connection(book_id) as conn:
        result = []
        for row in conn.execute("SELECT f.*,c.name AS character_name FROM character_forms f JOIN characters c ON c.id=f.character_id WHERE c.book_id=? ORDER BY c.name,f.start_ch,f.start_para,f.id", (book_id,)).fetchall():
            item = dict(row)
            item["card"] = json.loads(item.pop("card_json"))
            item["refs"] = [s.public_ref(book_id, r) for r in conn.execute("SELECT * FROM form_refs WHERE form_id=? ORDER BY created_at DESC,rowid DESC", (row["id"],)).fetchall()]
            result.append(item)
        return {"forms": result}


def form_values(payload, old=None):
    fields(payload, {"character_id", "form_key", "label", "card", "start_ch", "start_para", "reversible"})
    values = {"start_ch": 0, "start_para": 0, "reversible": True, **(dict(old) if old else {}), **payload}
    for key in ("start_ch", "start_para"):
        if isinstance(values[key], bool) or not isinstance(values[key], int) or not 0 <= values[key] <= 1000000:
            raise MangaError(key + " 须为非负整数")
    if "reversible" in payload and not isinstance(payload["reversible"], bool):
        raise MangaError("reversible 须为布尔值")
    for key in ("label", "form_key"):
        if not isinstance(values.get(key), str) or not 1 <= len(values[key].strip()) <= 100:
            raise MangaError(key + " 须为 1–100 字符")
    card = payload.get("card", json.loads(old["card_json"]) if old else {})
    if not isinstance(card, dict) or set(card) - set(CARD_FIELDS) or any(not isinstance(v, str) or len(v) > 500 for v in card.values()):
        raise MangaError("card 须为外貌字段组成的文本对象")
    values["card_json"] = stable_json(card)
    values["identity_block"] = render_identity(card, values["label"])
    values["identity_hash"] = identity_hash(values["identity_block"])
    return values


@router.post("/forms", status_code=201)
def create_form(book_id: str, payload: dict = Body(...)):
    values = form_values(payload)
    with connection(book_id) as conn:
        if not isinstance(payload.get("character_id"), str) or not conn.execute("SELECT 1 FROM characters WHERE id=? AND book_id=?", (payload["character_id"], book_id)).fetchone():
            raise MangaError("人物不存在", "character_not_found", 404)
        if conn.execute("SELECT 1 FROM character_forms WHERE character_id=? AND (form_key=? OR (start_ch=? AND start_para=?))", (payload["character_id"], values["form_key"], values["start_ch"], values["start_para"])).fetchone():
            raise MangaError("形态名称或生效锚点重复", "form_conflict", 409)
        form_id = s.uid("form_")
        conn.execute("INSERT INTO character_forms(id,character_id,form_key,label,card_json,identity_block,identity_hash,version,start_ch,start_para,reversible) VALUES(?,?,?,?,?,?,?,1,?,?,?)",
                     (form_id, payload["character_id"], values["form_key"], values["label"], values["card_json"], values["identity_block"], values["identity_hash"], values["start_ch"], values["start_para"], int(values["reversible"])))
        return {"id": form_id}


@router.patch("/forms/{form_id}")
def update_form(book_id: str, form_id: str, payload: dict = Body(...)):
    with connection(book_id) as conn:
        old = s.form_row(conn, book_id, form_id)
        if old["form_key"] == "base":
            raise MangaError("基础形态请在人物卡中编辑和升版", "edit_base_character", 409)
        if old["locked"]:
            raise MangaError("请先解冻形态再修改", "form_frozen", 409)
        fields(payload, {"label", "card", "start_ch", "start_para", "reversible"})
        v = form_values(payload, old)
        if conn.execute("SELECT 1 FROM character_forms WHERE character_id=? AND id!=? AND start_ch=? AND start_para=?", (old["character_id"], form_id, v["start_ch"], v["start_para"])).fetchone():
            raise MangaError("生效锚点重复", "form_conflict", 409)
        s.stale(conn, form_id)
        conn.execute("UPDATE character_forms SET label=?,card_json=?,identity_block=?,identity_hash=?,start_ch=?,start_para=?,reversible=? WHERE id=?", (v["label"], v["card_json"], v["identity_block"], v["identity_hash"], v["start_ch"], v["start_para"], int(v["reversible"]), form_id))
        return {"ok": True}


@router.post("/forms/{form_id}/actions/{action}")
def form_action(book_id: str, form_id: str, action: str, payload: dict = Body(default={})):
    fields(payload, {"confirm"})
    with connection(book_id) as conn:
        row = s.form_row(conn, book_id, form_id)
        if row["form_key"] == "base":
            raise MangaError("基础形态请使用人物卡的冻结/解冻操作", "edit_base_character", 409)
        if action == "freeze":
            conn.execute("UPDATE character_forms SET locked=1 WHERE id=?", (form_id,))
        elif action == "unfreeze" and payload.get("confirm") is True:
            if row["locked"]:
                s.stale(conn, form_id)
                conn.execute("UPDATE character_forms SET locked=0,version=version+1 WHERE id=?", (form_id,))
        else:
            raise MangaError("解冻须 confirm=true；仅支持 freeze/unfreeze")
        return {"ok": True}


@router.post("/forms/{form_id}/refs", status_code=201)
async def upload_ref(book_id: str, form_id: str, request: Request):
    raw = bytearray()
    async for chunk in request.stream():
        raw.extend(chunk)
        if len(raw) > 16 * 1024 * 1024:
            raise MangaError("图片超过 16 MB")
    with connection(book_id) as conn:
        row = s.form_row(conn, book_id, form_id)
        check_identity(row)
        ref_id = s.uid("ref_")
        path = s.save_image(raw, ref_id)
        conn.execute("INSERT INTO form_refs VALUES(?,?,?,?,?,?,?)", (ref_id, form_id, row["version"], row["identity_hash"], path, 0, int(time.time())))
        return {"ref": s.public_ref(book_id, conn.execute("SELECT * FROM form_refs WHERE id=?", (ref_id,)).fetchone())}


@router.patch("/refs/{ref_id}")
def review_ref(book_id: str, ref_id: str, payload: dict = Body(...)):
    fields(payload, {"accepted"})
    if not isinstance(payload.get("accepted"), bool):
        raise MangaError("accepted 须为布尔值")
    with connection(book_id) as conn:
        row = conn.execute("SELECT r.* FROM form_refs r JOIN character_forms f ON f.id=r.form_id JOIN characters c ON c.id=f.character_id WHERE r.id=? AND c.book_id=?", (ref_id, book_id)).fetchone()
        if not row:
            raise MangaError("参考图不存在", "ref_not_found", 404)
        form = s.form_row(conn, book_id, row["form_id"])
        if payload["accepted"]:
            check_identity(form)
            if form["version"] != row["form_version"] or form["identity_hash"] != row["identity_hash"]:
                raise MangaError("参考图与当前身份版本不一致", "reference_stale", 409)
        else:
            jobs = conn.execute("SELECT id,snapshot_json FROM render_jobs WHERE form_id=? AND status!='cancelled'", (form["id"],)).fetchall()
            for job in jobs:
                if (json.loads(job["snapshot_json"]).get("reference") or {}).get("id") == ref_id:
                    conn.execute("UPDATE render_jobs SET status='stale',error='参考图已撤销',updated_at=? WHERE id=?", (int(time.time()), job["id"]))
        conn.execute("UPDATE form_refs SET accepted=? WHERE id=?", (int(payload["accepted"]), ref_id))
        return {"ok": True}


@router.post("/jobs", status_code=201)
def create_job(book_id: str, payload: dict = Body(...)):
    with connection(book_id) as conn:
        return {"job": s.public_job(conn, s.enqueue(conn, book_id, payload))}


@router.get("/jobs")
def jobs(book_id: str, limit: int = Query(default=30, ge=1, le=100)):
    with connection(book_id) as conn:
        rows = conn.execute("SELECT * FROM render_jobs WHERE book_id=? ORDER BY created_at DESC,rowid DESC LIMIT ?", (book_id, limit)).fetchall()
        # Recheck scene/reference/identity changes, including M2 edits made outside this API.
        for row in rows:
            if row["status"] not in {"stale", "cancelled"}:
                try:
                    valid_snapshot(conn, row, json.loads(row["snapshot_json"]))
                except (ValueError, OSError) as exc:
                    conn.execute("UPDATE render_jobs SET status='stale',error=? WHERE id=?", (str(exc), row["id"]))
        return {"jobs": [s.public_job(conn, get_job(conn, book_id, row["id"])) for row in rows]}


@router.get("/jobs/{job_id}")
def job(book_id: str, job_id: str):
    with connection(book_id) as conn:
        return {"job": s.public_job(conn, get_job(conn, book_id, job_id))}


@router.post("/jobs/{job_id}/{action}")
def job_action(book_id: str, job_id: str, action: str, payload: dict = Body(default={})):
    fields(payload, {"seed"})
    with connection(book_id) as conn:
        row = get_job(conn, book_id, job_id)
        snap = json.loads(row["snapshot_json"])
        if action == "cancel":
            if row["status"] not in {"queued", "running", "error"}:
                raise MangaError("该任务已结束", "job_conflict", 409)
            conn.execute("UPDATE render_jobs SET status='cancelled',updated_at=? WHERE id=?", (int(time.time()), job_id))
        elif action == "approve":
            if row["status"] != "needs_review" or not row["image_path"] or row["kind"] != "panel":
                raise MangaError("仅可人工确认待复核的分镜；参考图请在参考图库确认", "job_conflict", 409)
            valid_snapshot(conn, row, snap)
            conn.execute("UPDATE render_jobs SET status='approved',error=NULL,updated_at=? WHERE id=?", (int(time.time()), job_id))
        elif action == "retry":
            if row["status"] != "error":
                raise MangaError("只有失败任务可以续查；换种子请用 reroll", "job_conflict", 409)
            valid_snapshot(conn, row, snap)
            conn.execute("UPDATE render_jobs SET status='queued',error=NULL,updated_at=? WHERE id=?", (int(time.time()), job_id))
        elif action == "reroll":
            if row["status"] in {"queued", "running"}:
                raise MangaError("请先等待或取消当前任务", "job_conflict", 409)
            request = {"kind": row["kind"], "form_id": row["form_id"], "shot": snap["shot"], "action": snap["action"], "flashback": snap["flashback"], "seed": payload.get("seed", (snap["seed"] + 7919) % (2**53))}
            if row["scene_id"]:
                request.update(scene_id=row["scene_id"], para=snap["para"])
            return {"job": s.public_job(conn, s.enqueue(conn, book_id, request))}
        elif action == "reference":
            if row["status"] not in {"done", "needs_review", "approved"} or not row["image_path"]:
                raise MangaError("请先完成出图，再选择参考图", "job_conflict", 409)
            valid_snapshot(conn, row, snap)
            ref_id = s.uid("ref_")
            path = s.save_image(s.asset_path(row["image_path"]).read_bytes(), ref_id)
            conn.execute("INSERT INTO form_refs VALUES(?,?,?,?,?,?,?)", (ref_id, row["form_id"], snap["form"]["version"], snap["form"]["identity_hash"], path, 0, int(time.time())))
            return {"ref": s.public_ref(book_id, conn.execute("SELECT * FROM form_refs WHERE id=?", (ref_id,)).fetchone())}
        else:
            raise MangaError("未知任务操作")
        return {"job": s.public_job(conn, get_job(conn, book_id, job_id))}


@router.get("/assets/{asset_id}")
def asset(book_id: str, asset_id: str):
    with connection(book_id) as conn:
        row = conn.execute("SELECT r.path FROM form_refs r JOIN character_forms f ON f.id=r.form_id JOIN characters c ON c.id=f.character_id WHERE r.id=? AND c.book_id=? UNION ALL SELECT image_path FROM render_jobs WHERE id=? AND book_id=? UNION ALL SELECT a.image_path FROM render_attempts a JOIN render_jobs j ON j.id=a.job_id WHERE a.id=? AND j.book_id=?", (asset_id, book_id, asset_id, book_id, asset_id, book_id)).fetchone()
        if not row or not row[0] or not s.asset_path(row[0]).is_file():
            raise MangaError("图片不存在", "asset_not_found", 404)
        return FileResponse(s.asset_path(row[0]), media_type="image/png", headers={"Cache-Control": "private, no-cache"})
