from __future__ import annotations

import asyncio
import json
import logging
import threading
import time

from .. import db, task_log
from ..ai.models import stable_json
from ..ai.retry import MAX_ATTEMPTS, RETRY_DELAY
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
    if snap.get("board_id"):
        from .storyboard import check_snapshot
        return check_snapshot(conn, job, snap)
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
        automatic = snap.get("automatic_draft") and conn.execute("SELECT 1 FROM auto_manga_refs WHERE ref_id=? AND book_id=?", (ref["id"], job["book_id"])).fetchone()
        if not row or (not row[0] and not automatic) or digest_bytes(asset_path(ref["path"]).read_bytes()) != ref["sha256"]:
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


def generate_attempt(conn, job, snap, provider, attempt_id, seed):
    cfg = snap["config"]
    existing = conn.execute("SELECT prompt_id FROM render_attempts WHERE id=?", (attempt_id,)).fetchone()[0]
    if cfg["provider"] == "openai-images" and existing:
        raise MangaError("上次图像 API 请求结果不确定，请核对服务端记录后再继续")
    auto = conn.execute("SELECT run_id FROM auto_manga WHERE book_id=?", (job["book_id"],)).fetchone()
    run_id = auto[0] if auto and snap.get("automatic_draft") else job["id"]
    report = task_log.reporter(job["book_id"], run_id, "rendering")
    label = "人物参考图" if job["kind"] == "reference" else "漫画分格"
    last_exc = None
    for number in range(1, MAX_ATTEMPTS + 1):
        if _stop.is_set() or conn.execute("SELECT status FROM render_jobs WHERE id=?", (job["id"],)).fetchone()[0] != "running":
            return None
        try:
            valid_snapshot(conn, job, snap)
        except (ValueError, OSError) as exc:
            status(conn, job["id"], "stale", error=str(exc))
            return None
        report(f"{label} · 第 {number}/{MAX_ATTEMPTS} 次尝试")
        submitting = False
        try:
            pid = conn.execute("SELECT prompt_id FROM render_attempts WHERE id=?", (attempt_id,)).fetchone()[0]
            ref = snap.get("reference")
            if cfg["provider"] == "openai-images":
                # Reuse one idempotency key throughout this request's retry budget.
                if not pid:
                    pid = uid("image_")
                    with conn:
                        conn.execute("UPDATE render_attempts SET prompt_id=?,workflow_json=? WHERE id=?", (pid, stable_json({"provider": cfg["provider"], "model": cfg["model"]}), attempt_id))
                refs = [asset_path(p["reference"]["path"]) for p in snap["cast"]] if snap.get("cast") else []
                raw = provider.generate(snap, refs if len(refs) > 1 else asset_path(ref["path"]) if ref else None, pid)
            elif provider:
                if not pid:
                    capabilities = provider.probe()
                    if not capabilities["ok"]:
                        raise MangaError("图像服务缺少所需节点或模型")
                    def persist_submission(prompt_id, graph):
                        with conn:
                            conn.execute("UPDATE render_attempts SET prompt_id=?,workflow_json=? WHERE id=?", (prompt_id, stable_json(graph), attempt_id))
                    submitting = True
                    pid, graph = provider.submit(snap, seed, asset_path(ref["path"]) if ref else None, "velmira/" + attempt_id, before_submit=persist_submission)
                    submitting = False
                    persist_submission(pid, graph)
                deadline = time.monotonic() + cfg["timeout"]
                raw = None
                while time.monotonic() < deadline:
                    if _stop.wait(1) or conn.execute("SELECT status FROM render_jobs WHERE id=?", (job["id"],)).fetchone()[0] != "running":
                        return None
                    raw = provider.result(pid)
                    if raw:
                        break
                if raw is None:
                    raise TimeoutError()
            else:
                raw = mock_image(seed, cfg["width"], cfg["height"])
            path = save_image(raw, attempt_id)
            report(label + " · 图片已保存")
            return path
        except Exception as exc:
            last_exc = exc
            # A known failed Comfy task can be submitted again. An uncertain
            # submission or a poll timeout must keep querying the same prompt.
            if cfg["provider"] == "comfyui" and isinstance(exc, MangaError) and (exc.code in {"render_failed", "invalid_workflow", "image_missing"} or submitting and exc.code == "comfy_error"):
                with conn:
                    conn.execute("UPDATE render_attempts SET prompt_id=NULL WHERE id=?", (attempt_id,))
            message = f"{label} · 第 {number}/{MAX_ATTEMPTS} 次失败" + ("，即将重试" if number < MAX_ATTEMPTS else "，已停止")
            with conn:
                conn.execute("UPDATE render_attempts SET error=? WHERE id=?", (message, attempt_id))
            report(message, "warning" if number < MAX_ATTEMPTS else "error")
            if number == MAX_ATTEMPTS:
                if isinstance(last_exc, MangaError):
                    raise MangaError("图像 AI 已尝试 3 次仍失败，请检查连接、模型和服务端任务记录",
                                     "image_retries_exhausted", 502,
                                     status_code=getattr(last_exc, "status_code", None),
                                     error_code=getattr(last_exc, "error_code", None) or getattr(last_exc, "code", None)) from None
                raise MangaError("图像 AI 已尝试 3 次仍失败，请检查连接、模型和服务端任务记录", "image_retries_exhausted", 502) from None
            if _stop.wait(RETRY_DELAY * number):
                return None


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
            path = generate_attempt(conn, job, snap, provider, attempt_id, seed)
            if path is None:
                return
            try:
                valid_snapshot(conn, job, snap)
            except (ValueError, OSError) as exc:
                status(conn, job_id, "stale", error=str(exc)); return
            result = {"status": "needs_review", "reason": "reference_requires_confirmation"}
            if job["kind"] == "panel":
                mode = snap.get("panel", {}).get("kind", "solo")
                if mode != "solo":
                    result = {"status": "needs_review", "reason": "双人画面须复核两人身份及互动" if mode == "duo" else "环境空镜须复核场景与原文"}
                else:
                    result = evaluate_face(asset_path(snap["reference"]["path"]), asset_path(path), cfg) if provider else {"status": "unavailable", "reason": "mock_output"}
            if finish_attempt(conn, job, snap, attempt_id, idx, path, result):
                return
        status(conn, job_id, "needs_review", error="重试次数已耗尽")
    except Exception as exc:
        log.warning("render job %s failed", job_id)
        from ..ai.diagnostics import provider_failure, public_failure
        diagnostic = provider_failure(exc)
        if diagnostic != "provider_failed":
            hint = public_failure(diagnostic)
            safe = "[" + diagnostic + "] " + hint["title"] + "；" + hint["detail"]
        else:
            safe = str(exc) if isinstance(exc, MangaError) else "图像 AI 请求异常；请检查连接、模型和服务端任务记录"
        safe = safe[:500]
        conn.execute("UPDATE render_attempts SET error=? WHERE job_id=? AND status='running'", (safe, job_id))
        status(conn, job_id, "error", error=safe)
    finally:
        conn.close()


async def loop():
    active = {}
    while not _stop.is_set():
        for key, task in list(active.items()):
            if task.done():
                try:
                    task.result()
                except Exception:
                    log.exception("image worker failed")
                del active[key]
        conn = db.get_conn()
        try:
            conn.execute("BEGIN IMMEDIATE")
            row = None
            for candidate in conn.execute("SELECT id,snapshot_json FROM render_jobs WHERE status='queued' ORDER BY created_at,rowid LIMIT 100"):
                cfg = json.loads(candidate["snapshot_json"])["config"]
                endpoint = (cfg["provider"], cfg["base_url"])
                capacity = cfg.get("max_concurrency", 1) if cfg["provider"] == "openai-images" else 1
                if cfg["provider"] == "openai-images":
                    from ..ai import routing
                    if routing.is_enabled("image"):
                        endpoint = ("openai-images", "model-pool")
                        capacity = routing.capacity("image")
                if len(active) < 8 and sum(k[1] == endpoint for k in active) < capacity:
                    row = candidate
                    break
            claimed = row and conn.execute("UPDATE render_jobs SET status='running',updated_at=? WHERE id=? AND status='queued'", (int(time.time()), row[0])).rowcount
            conn.commit()
        finally:
            conn.close()
        if claimed:
            active[(row[0], endpoint)] = asyncio.create_task(asyncio.to_thread(process, row[0]))
        else:
            await asyncio.sleep(.5)
    if active:
        await asyncio.gather(*active.values(), return_exceptions=True)


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
