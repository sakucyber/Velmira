"""Resume the structured storyboard path without additional per-panel text calls."""
import json
import time

from .. import task_log
from ..ai.models import stable_json
from . import storyboard, service as s, composition, pages
from .models import MangaError, digest


async def step(conn, job, state, scene, config_hash):
    from . import automatic as a
    book_id = job["book_id"]
    if not state.get("story"):
        if "page_ranges" not in state:
            from . import pagination
            with conn:
                a.save(conn, job, state, "planning", "正在按剧情节拍分页，每页最多7格")
            ranges = await pagination.plan(book_id, scene["id"], guard=lambda: a.guard(conn, job),
                report=task_log.reporter(book_id, job["run_id"], "planning"))
            with conn:
                state.update(page_ranges=ranges, page_cursor=0)
                a.save(conn, job, state, "planning", "分页已保存，正在编排本页画格")
            if not ranges:
                with conn:
                    state.pop("page_ranges", None)
                    state["scene"] = state.get("scene", 0) + 1
                    a.save(conn, job, state, "queued", "本段无正文，继续下一段")
                return
        page_range = state["page_ranges"][state.get("page_cursor", 0)]
        with conn:
            a.save(conn, job, state, "planning", "正在一次编排整页分镜、动作和环境")
        board = await storyboard.create(book_id, scene["id"], previous=state.get("previous", []),
                    page_range=page_range,
                    guard=lambda: a.guard(conn, job), report=task_log.reporter(book_id, job["run_id"], "planning"))
        with conn:
            state.update(story=board["panels"], board_id=board["id"], board_revision=board["revision"], panel=0,
                         scene_id=scene["id"], config=config_hash, scene_hash=digest(dict(scene)))
            a.save(conn, job, state, "generating", "整页分镜已保存，无需逐格重复调用文本模型")
        return
    board = storyboard.get(conn, book_id, state["board_id"])
    if state["board_revision"] != board["revision"]:
        if any(p.get("job_id") for p in state["story"]):
            raise MangaError("已开始出图的自动页不能直接更换分镜，请处理生成记录")
        state.update(story=board["panels"], board_revision=board["revision"], panel=0)
    if s.configuration(conn, book_id).get("review_storyboards") and state.get("approved_revision") != board["revision"]:
        with conn:
            a.save(conn, job, state, "reviewing", "分镜已就绪，请预览、修改并确认本页后开始出图")
        return
    story = state["story"]
    # Enqueue every ready panel; the image worker applies endpoint concurrency/RPM limits.
    for index, panel in enumerate(story):
        if panel.get("job_id"):
            if a.job_ready(conn, panel["job_id"]) is None:
                continue
            continue
        with conn:
            a.guard(conn, job)
            ids = panel.setdefault("form_ids", [])
            for i, subject in enumerate(panel["subjects"]):
                if len(ids) <= i:
                    span = storyboard.source_span(board["material"], panel)
                    chapter_row = conn.execute("SELECT paras_json FROM chapters WHERE book_id=? AND idx=?", (book_id, job["chapter"])).fetchone()
                    para, offset = storyboard.locate(json.loads(chapter_row[0]), span["end"])
                    ids.append(a.choose_form(conn, book_id, subject["character_id"], job["chapter"], para, source_offset=span["end"]))
                fid = ids[i]
                if a.reference_ready(conn, book_id, fid):
                    continue
                refs = panel.setdefault("ref_jobs", {})
                if fid not in refs:
                    refs[fid] = s.enqueue(conn, book_id, {"kind": "reference", "form_id": fid,
                        "request_id": job["run_id"] + ":ref:" + fid}, automatic=True)["id"]
                ref_job = a.job_ready(conn, refs[fid])
                if ref_job:
                    ref = conn.execute("SELECT id FROM form_refs WHERE id IN (SELECT id FROM render_attempts WHERE job_id=?) ORDER BY created_at,id LIMIT 1", (ref_job["id"],)).fetchone()
                    if not ref:
                        raise MangaError("人物参考图尚未保存")
                    conn.execute("INSERT OR IGNORE INTO auto_manga_refs VALUES(?,?)", (ref[0], book_id))
                a.save(conn, job, state, "reference", "正在为本页人物准备参考图")
                return
            panel["job_id"] = storyboard.enqueue(conn, book_id, board["id"], index, form_ids=ids, automatic=True)["id"]
            a.save(conn, job, state, "generating", f"第 {index + 1}/{len(story)} 格已排队")
    complete = sum(a.job_ready(conn, p["job_id"]) is not None for p in story)
    state["panel"] = complete
    if complete < len(story):
        with conn:
            a.save(conn, job, state, "generating", f"本页已完成 {complete}/{len(story)} 格")
        return
    captured = a.captured_sources(conn, book_id, story)
    title = conn.execute("SELECT title FROM chapters WHERE book_id=? AND idx=?", (book_id, job["chapter"])).fetchone()[0]
    layout = (board["material"].get("page_range") or {}).get("layout", "auto")
    if composition.LAYOUTS.get(layout) != len(story):
        layout = "auto"
    spec = composition.validate({"title": str(title)[:100], "layout": layout,
        "panels": [{"job_id": p["job_id"], "source_span": storyboard.source_span(board["material"], p),
                    **{k: p[k] for k in ("caption", "dialogue", "speaker")}} for p in story]})
    import asyncio
    path, image_hash = await asyncio.to_thread(pages.render, spec, captured)
    try:
        with conn:
            conn.execute("BEGIN IMMEDIATE")
            a.guard(conn, job)
            if a.captured_sources(conn, book_id, story) != captured:
                raise MangaError("拼页期间分镜发生变化")
            count = conn.execute("SELECT COUNT(*) FROM auto_manga_pages WHERE book_id=? AND run_id=?", (book_id, job["run_id"])).fetchone()[0]
            # The legacy uniqueness key is scene_id. A source-bound board ID
            # distinguishes multiple pages of the same scene without rewriting
            # old drafts or requiring a destructive migration.
            page_scene = scene["id"] + ":" + board["id"] if state.get("page_ranges") else scene["id"]
            conn.execute("INSERT INTO auto_manga_pages VALUES(?,?,?,?,?,?,?,?,?,?,?)", (s.uid("autopage_"), book_id, job["run_id"], page_scene, job["chapter"], count, stable_json(spec), stable_json(captured), path, image_hash, int(time.time())))
            more = state.get("page_cursor", 0) + 1 < len(state.get("page_ranges", []))
            next_state = {"completed": state.get("completed", 0), "scene": state.get("scene", 0) + (0 if more else 1),
                          "previous": [{k: p[k] for k in ("caption", "action", "subjects")} for p in story], "analysis_run": state.get("analysis_run")}
            if more:
                next_state.update(page_ranges=state["page_ranges"], page_cursor=state.get("page_cursor", 0) + 1)
            a.save(conn, job, next_state, "queued", "漫画页已保存，继续下一段情节")
    except BaseException:
        s.asset_path(path).unlink(missing_ok=True)
        raise
