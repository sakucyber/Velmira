"""Automatic comics remain drafts; they never claim human approval or replace M4 editions."""
import json

from fastapi import APIRouter, Body, Query
from fastapi.responses import FileResponse

from ..manga import automatic as a, pages, service as s
from ..manga.models import MangaError
from .manga import connection, fields

router = APIRouter(prefix="/api/books/{book_id}/manga/auto")


@router.get("")
def status(book_id: str):
    with connection(book_id) as conn:
        return {"auto": a.overview(conn, book_id)}


@router.patch("")
def toggle(book_id: str, payload: dict = Body(...)):
    fields(payload, {"enabled", "chapter"})
    if not isinstance(payload.get("enabled"), bool):
        raise MangaError("enabled 须为布尔值")
    chapter = payload.get("chapter", 0)
    if isinstance(chapter, bool) or not isinstance(chapter, int) or chapter < 0:
        raise MangaError("chapter 须为非负整数")
    with connection(book_id) as conn:
        return {"auto": a.toggle(conn, book_id, payload["enabled"], chapter)}


_ERROR_HINTS = {
    "image_api_failed": ("图像 API 返回失败", "请在模型库检查图像模型、接口地址和密钥，然后重新诊断。", "image"),
    "image_routing_exhausted": ("图像模型池暂时都不可用", "检查备用模型是否可访问，或稍后重试。", "image"),
    "image_retries_exhausted": ("图像生成重试次数已用尽", "检查图像服务日志和尺寸/参考图能力，再继续生成。", "image"),
    "comfy_unavailable": ("本机 ComfyUI 无法连接", "启动 ComfyUI 并确认地址可访问，再继续自动漫画。", "comfyui"),
    "comfy_error": ("ComfyUI 返回错误", "打开 ComfyUI 任务记录，修正节点或模型配置后重试。", "comfyui"),
    "invalid_workflow": ("ComfyUI 工作流配置有误", "检查 UNet、CLIP、VAE 和编辑 LoRA 是否存在。", "comfyui"),
    "render_failed": ("图像服务执行失败", "查看图像服务任务记录，确认余额、模型能力和参考图格式。", "image"),
    "invalid_text_output": ("文本模型输出不符合分镜格式", "切换文本模型或恢复默认提示词后重试。", "text"),
    "auto_config_changed": ("当前页的模型或画风配置发生变化", "恢复原配置，或从当前章节重新开始生成。", "config"),
    "auto_source_changed": ("小说正文发生变化", "重新导入为新书后再开启自动漫画，已有草稿会保留。", "source"),
    "auto_render_stopped": ("有出图任务需要人工处理", "打开漫画生成记录，处理失败或待复核任务后继续。", "jobs"),
    "automatic_failed": ("自动漫画任务中断", "检查网络、模型输出和磁盘空间后继续。", "general"),
}


@router.get("/diagnose")
def diagnose(book_id: str):
    """Return actionable, user-facing checks without making provider requests."""
    with connection(book_id) as conn:
        auto = a.overview(conn, book_id)
        code = auto.get("error_code")
        checks = []
        from ..ai.diagnostics import public_failure
        if code in {"provider_auth", "provider_forbidden", "provider_billing", "provider_rate_limit",
                    "provider_server_error", "provider_timeout", "provider_offline", "provider_invalid_response", "provider_failed"}:
            hint = public_failure(code)
            checks.append({"level": "error", **hint})
        elif code in _ERROR_HINTS:
            title, detail, kind = _ERROR_HINTS[code]
            checks.append({"level": "error", "code": code, "title": title, "detail": detail, "kind": kind})
        elif auto.get("status") == "error":
            checks.append({"level": "error", "code": "automatic_failed", "title": "自动漫画任务中断", "detail": "请检查模型连接和任务记录后继续。", "kind": "general"})
        elif auto.get("status") == "paused":
            checks.append({"level": "warning", "code": "paused", "title": "自动漫画已暂停", "detail": "可以从已保存的章节位置继续，已生成的草稿会保留。", "kind": "resume"})
        else:
            checks.append({"level": "ok", "code": "status_ok", "title": "任务状态正常", "detail": "没有发现需要处理的自动漫画错误。", "kind": "status"})

        # A diagnosis response is deliberately a public summary, not a copy of
        # persisted technical text (older data may predate redacted providers).
        auto["message"] = checks[0]["title"]
        auto["error_code"] = checks[0]["code"] if checks[0]["level"] == "error" else None
        latest = conn.execute("SELECT stage,level,created_at FROM task_logs WHERE book_id=? ORDER BY id DESC LIMIT 5", (book_id,)).fetchall()
        stages = {"analysis": "正在分析章节", "planning": "正在编排分镜", "rendering": "正在生成图片", "reference": "正在准备人物参考图", "error": "任务出现错误"}
        recent = [{"stage": item["stage"] if item["stage"] in stages else "status", "level": item["level"] if item["level"] in {"info", "warning", "error"} else "info", "created_at": item["created_at"],
                   "message": "服务响应不稳定，系统正在重试" if item["level"] == "warning" else stages.get(item["stage"], "任务状态已更新")}
                  for item in latest]
        failed_jobs = conn.execute("SELECT COUNT(*) FROM render_jobs WHERE book_id=? AND status='error'", (book_id,)).fetchone()[0]
        if failed_jobs:
            checks.append({"level": "warning", "code": "render_jobs_failed", "title": "存在失败的出图任务", "detail": f"有 {failed_jobs} 个任务失败，可在漫画生成记录中查看具体错误。", "kind": "jobs"})
        return {"auto": auto, "checks": checks, "recent_logs": recent,
                "can_resume": auto.get("status") in {"error", "paused"} and auto.get("start_chapter") is not None}


def check_draft(conn, book_id, row):
    pending = 0
    for source in json.loads(row["sources_json"]):
        if source["job_id"]:
            job = conn.execute("SELECT * FROM render_jobs WHERE id=? AND book_id=?", (source["job_id"], book_id)).fetchone()
            if not job:
                raise MangaError("分镜已不存在")
            if a.job_ready(conn, job["id"]) is None:
                raise MangaError("分镜正在重新生成")
            pending += job["status"] == "needs_review"
        path = s.asset_path(source["image_path"])
        if not path.is_file() or pages.file_hash(path) != source["image_hash"]:
            raise MangaError("分镜图片丢失或发生变化")
    target = s.asset_path(row["image_path"])
    if not target.is_file() or pages.file_hash(target) != row["image_hash"]:
        raise MangaError("漫画页图片丢失或发生变化")
    return pending


@router.get("/runs")
def runs(book_id: str):
    with pages.connection(book_id) as conn:
        return {"runs": [dict(r) for r in conn.execute("SELECT run_id,MIN(chapter) AS start_chapter,COUNT(*) AS pages,MIN(created_at) AS created_at FROM auto_manga_pages WHERE book_id=? GROUP BY run_id ORDER BY created_at DESC,run_id LIMIT 100", (book_id,))]}


@router.get("/pages")
def list_pages(book_id: str, offset: int = Query(0, ge=0), limit: int = Query(12, ge=1, le=30), run_id: str | None = None, chapter: int | None = Query(None, ge=0)):
    with pages.connection(book_id) as conn:
        current = a.row(conn, book_id)
        run_id = run_id or (current["run_id"] if current else "")
        where = "book_id=? AND run_id=?" + (" AND chapter=?" if chapter is not None else "")
        args = (book_id, run_id, chapter) if chapter is not None else (book_id, run_id)
        total = conn.execute("SELECT COUNT(*) FROM auto_manga_pages WHERE " + where, args).fetchone()[0]
        result = []
        for row in conn.execute("SELECT * FROM auto_manga_pages WHERE " + where + " ORDER BY ordinal LIMIT ? OFFSET ?", (*args, limit, offset)):
            error, pending = None, 0
            try:
                pending = check_draft(conn, book_id, row)
            except (ValueError, OSError) as exc:
                error = str(exc)
            result.append({"id": row["id"], "chapter": row["chapter"], "ordinal": row["ordinal"],
                           "title": json.loads(row["spec_json"])["title"], "status": "stale" if error else "draft",
                           "pending_review": pending, "error": error,
                           "url": f"/api/books/{book_id}/manga/auto/pages/{row['id']}/image"})
        return {"pages": result, "total": total, "has_more": offset + len(result) < total}


@router.get("/logs")
def logs(book_id: str, after: int = Query(0, ge=0), limit: int = Query(100, ge=1, le=200), run_id: str | None = None):
    with connection(book_id) as conn:
        job = a.row(conn, book_id)
        current = job["run_id"] if job else ""
        selected = run_id or current
        runs = [selected]
        if selected == current:
            analysis = conn.execute("SELECT run_id FROM book_analysis WHERE book_id=?", (book_id,)).fetchone()
            if analysis:
                runs.append(analysis[0])
        marks = ",".join("?" for _ in runs)
        query = f"SELECT id,stage,level,message,created_at FROM task_logs WHERE book_id=? AND run_id IN ({marks}) AND id>? ORDER BY id"
        # First load shows the latest bounded window; later loads follow its cursor.
        if not after:
            query += " DESC LIMIT ?"
        else:
            query += " LIMIT ?"
        entries = [dict(r) for r in conn.execute(query, (book_id, *runs, after, limit))]
        if not after:
            entries.reverse()
        return {"logs": entries, "run_id": selected, "cursor": entries[-1]["id"] if entries else after}


@router.get("/pages/{page_id}/image")
def image(book_id: str, page_id: str):
    with pages.connection(book_id) as conn:
        row = conn.execute("SELECT * FROM auto_manga_pages WHERE id=? AND book_id=?", (page_id, book_id)).fetchone()
        if not row:
            raise MangaError("自动漫画页不存在", "auto_page_not_found", 404)
        path = s.asset_path(row["image_path"])
        if not path.is_file():
            raise MangaError("漫画页图片丢失", "image_missing", 404)
    return FileResponse(path, media_type="image/png", headers={"Cache-Control": "private, no-cache"})
