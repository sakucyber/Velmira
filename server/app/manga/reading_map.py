"""Read-only, source-verified correspondence between prose and rendered panels."""
from __future__ import annotations

import json

from . import composition, service
from .models import digest


def reading_map(conn, book_id, chapter=None, run_id=None):
    service.book(conn, book_id)
    current = conn.execute("SELECT run_id FROM auto_manga WHERE book_id=?", (book_id,)).fetchone()
    selected_run = run_id or (current[0] if current else None)
    chapter_rows = conn.execute("SELECT idx,title,paras_json FROM chapters WHERE book_id=?" + (" AND idx=?" if chapter is not None else ""),
                                (book_id, chapter) if chapter is not None else (book_id,)).fetchall()
    texts = {row["idx"]: "\n".join(json.loads(row["paras_json"])) for row in chapter_rows}
    chapter_hashes = {row["idx"]: digest({"title":row["title"], "paras_json":row["paras_json"]}) for row in chapter_rows}
    rows = [(False, row) for row in conn.execute("SELECT * FROM manga_pages WHERE book_id=? AND deleted=0 ORDER BY position,id", (book_id,))]
    if selected_run:
        rows += [(True, row) for row in conn.execute("SELECT * FROM auto_manga_pages WHERE book_id=? AND run_id=?" +
                (" AND chapter=?" if chapter is not None else "") + " ORDER BY ordinal,id",
                (book_id, selected_run, chapter) if chapter is not None else (book_id, selected_run))]
    panels, seen, boards = [], set(), {}
    for draft, row in rows:
        spec = json.loads(row["spec_json"])
        sources = json.loads(row["sources_json"])
        boxes = composition.rectangles(spec)
        page_url = (f"/api/books/{book_id}/manga/auto/pages/{row['id']}/image" if draft else
                    f"/api/books/{book_id}/manga/pages/{row['id']}/image?revision={row['revision']}")
        if not service.asset_path(row["image_path"]).is_file():
            continue
        for index, (panel, source, box) in enumerate(zip(spec.get("panels", []), sources, boxes)):
            job_id = source.get("job_id") or panel.get("job_id")
            if not job_id or job_id in seen:
                continue
            job = conn.execute("SELECT * FROM render_jobs WHERE id=? AND book_id=?", (job_id, book_id)).fetchone()
            if not job or job["kind"] != "panel" or job["status"] not in {"done", "approved", "needs_review"} or not job["image_path"]:
                continue
            snap = json.loads(job["snapshot_json"])
            span = snap.get("source_span") or source.get("source_span") or panel.get("source_span")
            if not isinstance(span, dict) or type(span.get("chapter")) is not int or span["chapter"] not in texts:
                continue
            # A matching short quote alone cannot certify the surrounding
            # chapter stayed the same. Read each board only once, and verify
            # its captured chapter fingerprint without rebuilding material or
            # performing reference/memory queries per panel.
            board_id = snap.get("board_id")
            expected_hash = snap.get("chapter_hash")
            if board_id:
                if board_id not in boards:
                    board = conn.execute("SELECT plan_json FROM storyboards WHERE id=? AND book_id=?", (board_id, book_id)).fetchone()
                    boards[board_id] = json.loads(board[0]) if board else None
                board = boards[board_id]
                if not board:
                    continue
                material = board.get("material", {})
                if material.get("chapter") != span["chapter"]:
                    continue
                expected_hash = material.get("chapter_hash")
                if snap.get("source_hash") and snap["source_hash"] != board.get("source_hash"):
                    continue
            if not expected_hash or expected_hash != chapter_hashes[span["chapter"]]:
                continue
            text = texts[span["chapter"]]
            start, end = span.get("start"), span.get("end")
            if type(start) is not int or type(end) is not int or not 0 <= start < end <= len(text):
                continue
            quote = span.get("quote", "")
            if not isinstance(quote, str) or not quote or text[start:end] != quote:
                continue
            if not service.asset_path(job["image_path"]).is_file():
                continue
            x, y, right, bottom = box
            panels.append({"chapter": span["chapter"], "start": start, "end": end,
                "source_quote": quote or text[start:end], "job_id": job_id, "page_id": row["id"],
                "panel_index": index, "page_url": page_url,
                "url": f"/api/books/{book_id}/manga/assets/{job_id}",
                "rect": [x / spec["width"], y / spec["height"], (right-x) / spec["width"], (bottom-y) / spec["height"]],
                "caption": panel.get("caption", ""), "dialogue": panel.get("dialogue", ""),
                "speaker": panel.get("speaker", ""), "draft": draft,
                "pending_review": job["status"] == "needs_review"})
            seen.add(job_id)
    panels.sort(key=lambda item: (item["chapter"], item["start"], item["end"], item["panel_index"]))
    return {"book_id": book_id, "run_id": selected_run, "offset_unit": "unicode_codepoints",
            "range_end": "exclusive", "panels": panels}
