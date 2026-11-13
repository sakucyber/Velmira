from __future__ import annotations

import asyncio
import json
import logging
import threading
import time

from .. import db
from ..ai.models import stable_json
from .face import evaluate_face
from .models import MangaError, check_identity
from .provider import ComfyProvider, ImageAPIProvider, mock_image
from .service import asset_path, digest, digest_bytes, save_image, uid

log = logging.getLogger("velmira.manga")
_stop = threading.Event()
_task = None


def finish_attempt(conn, job, snap, attempt_id, idx, path, result):
    """Commit image, evaluation and final state together; replay safely after a restart."""
    conn.execute("BEGIN IMMEDIATE")
    if conn.execute("SELECT status FROM render_jobs WHERE id=?", (job["id"],)).fetchone()[0] != "running":
        conn.rollback()
        return True
    try:
        valid_snapshot(conn, job, snap)
    except (ValueError, OSError) as exc:
        status(conn, job["id"], "stale", error=str(exc))
        return True
    score = result.get("score")
    conn.execute("UPDATE render_attempts SET status='done',image_path=?,result_json=?,error=NULL WHERE id=?", (path, stable_json(result), attempt_id))
    conn.execute("UPDATE render_jobs SET image_path=?,face_score=? WHERE id=?", (path, score, job["id"]))
    if job["kind"] == "reference":
        conn.execute("INSERT OR IGNORE INTO form_refs VALUES(?,?,?,?,?,?,?)", (attempt_id, job["form_id"], snap["form"]["version"], snap["form"]["identity_hash"], path, 0, int(time.time())))
        status(conn, job["id"], "needs_review", error="参考图待人工确认")
        return True
    if result["status"] == "measured" and score is not None and score >= snap["config"]["face_threshold"]:
        status(conn, job["id"], "done", error=None)
        return True
    if result["status"] != "measured" or idx == snap["config"]["max_attempts"]:
        status(conn, job["id"], "needs_review", error=result.get("reason") or "人脸相似度未达阈值")
        return True
    conn.commit()
    return False


def status(conn, job_id, value, **fields):
    fields.update(status=value, updated_at=int(time.time()))
    conn.execute("UPDATE render_jobs SET " + ",".join(k + "=?" for k in fields) + " WHERE id=? AND status='running'", (*fields.values(), job_id))
    conn.commit()


def valid_snapshot(conn, job, snap):
    form = conn.execute("SELECT * FROM character_forms WHERE id=?", (job["form_id"],)).fetchone()
    check_identity(form)
    character = conn.execute("SELECT frozen,identity_hash,version FROM characters WHERE id=?", (form["character_id"],)).fetchone()
    if form["form_key"] == "base" and (not character["frozen"] or character["identity_hash"] != form["identity_hash"] or character["version"] != form["version"]):
        raise MangaError("基础身份已改变", "render_stale", 409)
    if form["version"] != snap["form"]["version"] or form["identity_hash"] != snap["form"]["identity_hash"]:
        raise MangaError("形态版本已改变", "render_stale", 409)
    ref = snap.get("reference")
    if ref:
        row = conn.execute("SELECT accepted FROM form_refs WHERE id=?", (ref["id"],)).fetchone()
        if not row or not row[0] or digest_bytes(asset_path(ref["path"]).read_bytes()) != ref["sha256"]:
            raise MangaError("参考图已撤销或文件发生变化", "render_stale", 409)
    if job["scene_id"]:
        scene = conn.execute("SELECT plan_json,status FROM scenes WHERE id=?", (job["scene_id"],)).fetchone()
        if not scene or scene["status"] == "needs_review" or digest(json.loads(scene[0])) != snap["scene_hash"]:
            raise MangaError("场景已改变", "render_stale", 409)
    if snap.get("text_plan"):
        from ..ai.director import prepare_material
        payload = {"scene_id": job["scene_id"], "form_id": job["form_id"], "para": snap["para"],
                   "shot": snap["shot"], "action": snap["action"], "flashback": snap["flashback"]}
        if digest(prepare_material(conn, job["book_id"], payload, snap["config"])) != snap["text_plan"]["source_hash"]:
            raise MangaError("用于总结的章节上下文已变化", "render_stale", 409)


def process(job_id):
    conn = db.get_conn()
    try:
        job = conn.execute("SELECT * FROM render_jobs WHERE id=?", (job_id,)).fetchone()
        snap = json.loads(job["snapshot_json"]); cfg = snap["config"]
        try:
            valid_snapshot(conn, job, snap)
        except (ValueError, OSError) as exc:
            status(conn, job_id, "stale", error=str(exc)); return
        provider = ComfyProvider(cfg) if cfg["provider"] == "comfyui" else ImageAPIProvider(cfg) if cfg["provider"] == "openai-images" else None
        for idx in range(1, cfg["max_attempts"] + 1):
            if _stop.is_set():
                return
            if conn.execute("SELECT status FROM render_jobs WHERE id=?", (job_id,)).fetchone()[0] != "running":
                return
            attempt = conn.execute("SELECT * FROM render_attempts WHERE job_id=? AND idx=?", (job_id, idx)).fetchone()
            if attempt and attempt["status"] == "done":
                if finish_attempt(conn, job, snap, attempt["id"], idx, attempt["image_path"], json.loads(attempt["result_json"])):
                    return
                continue
            attempt_id = attempt["id"] if attempt else uid("attempt_")
            seed = (snap["seed"] + (idx - 1) * 1009) % (2**53)
            if not attempt:
                conn.execute("INSERT INTO render_attempts(id,job_id,idx,seed) VALUES(?,?,?,?)", (attempt_id, job_id, idx, seed))
                conn.execute("UPDATE render_jobs SET attempts=? WHERE id=?", (idx, job_id)); conn.commit()
            pid = attempt["prompt_id"] if attempt else None
            if cfg["provider"] == "openai-images":
                if pid:
                    raise MangaError("上次图像 API 请求结果不确定；请先核对服务端记录，再用“换种子重画”新建任务，避免重复计费")
                pid = uid("image_")
                conn.execute("UPDATE render_attempts SET prompt_id=?,workflow_json=? WHERE id=?", (pid, stable_json({"provider": cfg["provider"], "model": cfg["model"]}), attempt_id)); conn.commit()
                ref = snap.get("reference")
                raw = provider.generate(snap, asset_path(ref["path"]) if ref else None, pid)
            elif provider:
                if not pid:
                    capabilities = provider.probe()
                    if not capabilities["ok"]:
                        raise ValueError("缺少节点或模型：" + ", ".join(capabilities["missing"]))
                    ref = snap.get("reference")
                    def persist_submission(prompt_id, graph):
                        conn.execute("UPDATE render_attempts SET prompt_id=?,workflow_json=? WHERE id=?", (prompt_id, stable_json(graph), attempt_id))
                        conn.commit()
                    pid, graph = provider.submit(snap, seed, asset_path(ref["path"]) if ref else None, "velmira/" + attempt_id, before_submit=persist_submission)
                    conn.execute("UPDATE render_attempts SET prompt_id=?,workflow_json=? WHERE id=?", (pid, stable_json(graph), attempt_id)); conn.commit()
                deadline = time.monotonic() + cfg["timeout"]
                raw = None
                while time.monotonic() < deadline:
                    if _stop.wait(1):
                        return
                    if conn.execute("SELECT status FROM render_jobs WHERE id=?", (job_id,)).fetchone()[0] != "running":
                        return
                    try:
                        raw = provider.result(pid)
                    except MangaError as exc:
                        if exc.code == "comfy_unavailable":
                            continue
                        raise
                    if raw:
                        break
                if raw is None:
                    raise TimeoutError("生成超时；重试会继续查询同一任务，不重复提交")
            else:
                raw = mock_image(seed, cfg["width"], cfg["height"])
            path = save_image(raw, attempt_id)
            try:
                valid_snapshot(conn, job, snap)
            except (ValueError, OSError) as exc:
                status(conn, job_id, "stale", error=str(exc)); return
            result = {"status": "needs_review", "reason": "reference_requires_confirmation"}
            if job["kind"] == "panel":
                result = evaluate_face(asset_path(snap["reference"]["path"]), asset_path(path), cfg) if provider else {"status": "unavailable", "reason": "mock_output"}
            if finish_attempt(conn, job, snap, attempt_id, idx, path, result):
                return
        status(conn, job_id, "needs_review", error="重试次数已耗尽")
    except Exception as exc:
        log.warning("render job %s failed: %s", job_id, exc)
        conn.execute("UPDATE render_attempts SET error=? WHERE job_id=? AND status='running'", (str(exc)[:2000], job_id))
        status(conn, job_id, "error", error=str(exc)[:2000])
    finally:
        conn.close()


async def loop():
    while not _stop.is_set():
        conn = db.get_conn()
        try:
            conn.execute("BEGIN IMMEDIATE")
            row = conn.execute("SELECT id FROM render_jobs WHERE status='queued' ORDER BY created_at,rowid LIMIT 1").fetchone()
            claimed = row and conn.execute("UPDATE render_jobs SET status='running',updated_at=? WHERE id=? AND status='queued'", (int(time.time()), row[0])).rowcount
            conn.commit()
        finally:
            conn.close()
        if claimed:
            await asyncio.to_thread(process, row[0])
        else:
            await asyncio.sleep(.5)


def start():
    global _task
    _stop.clear()
    conn = db.get_conn()
    try:
        conn.execute("UPDATE render_jobs SET status='queued' WHERE status='running'"); conn.commit()
    finally:
        conn.close()
    _task = asyncio.create_task(loop())


async def stop():
    _stop.set()
    if _task:
        await _task
