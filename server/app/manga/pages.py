"""M4 editable page revisions, source checks and immutable CBZ editions."""
from __future__ import annotations

import hashlib
import json
import re
import time
import zipfile
from contextlib import contextmanager
from xml.etree import ElementTree as ET

from .. import db
from ..ai.models import stable_json
from . import composition as c, service as s
from .models import MangaError, digest
from .queue import valid_snapshot


@contextmanager
def connection(book_id, write=False):
    conn = db.get_conn()
    try:
        if write:
            conn.execute("BEGIN IMMEDIATE")
        s.book(conn, book_id)
        yield conn
        conn.commit()
    except BaseException:
        conn.rollback(); raise
    finally:
        conn.close()


def file_hash(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def sources(conn, book_id, spec):
    result = []
    for panel in spec["panels"]:
        row = conn.execute("SELECT * FROM render_jobs WHERE id=? AND book_id=?", (panel["job_id"], book_id)).fetchone()
        if not row or row["kind"] != "panel" or not row["image_path"]:
            raise MangaError("请选择本书已生成的分镜图", "panel_not_found", 404)
        if row["status"] not in {"done", "approved"}:
            raise MangaError("分镜须通过一致性检查或人工确认后才能成页", "panel_not_reviewed", 409)
        snap = json.loads(row["snapshot_json"])
        valid_snapshot(conn, row, snap)
        path = s.asset_path(row["image_path"])
        if not path.is_file():
            raise MangaError("分镜图片已丢失", "panel_image_missing", 409)
        result.append({"job_id": row["id"], "image_path": row["image_path"], "image_hash": file_hash(path),
                       "config_hash": row["config_hash"], "chapter": snap["chapter"], "para": snap["para"]})
    return result


def page_row(conn, book_id, page_id, deleted=False):
    row = conn.execute("SELECT * FROM manga_pages WHERE id=? AND book_id=?", (page_id, book_id)).fetchone()
    if not row or (row["deleted"] and not deleted):
        raise MangaError("漫画页不存在", "manga_page_not_found", 404)
    return row


def check_page(conn, row):
    spec = json.loads(row["spec_json"])
    if sources(conn, row["book_id"], spec) != json.loads(row["sources_json"]):
        raise MangaError("分镜图或身份已变化，请重新保存漫画页", "manga_page_stale", 409)
    path = s.asset_path(row["image_path"])
    if not path.is_file() or file_hash(path) != row["image_hash"]:
        raise MangaError("漫画页图片丢失或已变化，请重新保存", "manga_page_stale", 409)


def public_page(conn, row, verify=True):
    error = None
    if verify:
        try:
            check_page(conn, row)
        except (ValueError, OSError) as exc:
            error = str(exc)
    return {"id": row["id"], "position": row["position"], "revision": row["revision"], "deleted": bool(row["deleted"]),
            "spec": json.loads(row["spec_json"]), "status": "stale" if error else "ready", "error": error,
            "url": f'/api/books/{row["book_id"]}/manga/pages/{row["id"]}/image?revision={row["revision"]}',
            "updated_at": row["updated_at"]}


def render(spec, source):
    relative = "assets/pages/" + s.uid("page_image_") + ".png"
    target = s.asset_path(relative)
    try:
        c.compose(spec, [s.asset_path(item["image_path"]) for item in source], target)
        return relative, file_hash(target)
    except BaseException:
        target.unlink(missing_ok=True); raise


def create_pages(book_id, specs):
    specs = [c.validate(spec) for spec in specs]
    with connection(book_id) as conn:
        captured = [sources(conn, book_id, spec) for spec in specs]
    rendered = []
    try:
        for spec, source in zip(specs, captured):
            rendered.append(render(spec, source))
        with connection(book_id, True) as conn:
            position = conn.execute("SELECT COALESCE(MAX(position),-1)+1 FROM manga_pages WHERE book_id=? AND deleted=0", (book_id,)).fetchone()[0]
            ids = []
            for spec, source, (path, image_hash) in zip(specs, captured, rendered):
                if sources(conn, book_id, spec) != source:
                    raise MangaError("拼页期间分镜已变化，请重试", "manga_page_stale", 409)
                pid, now = s.uid("page_"), int(time.time())
                conn.execute("INSERT INTO manga_pages(id,book_id,position,spec_json,sources_json,image_path,image_hash,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
                             (pid, book_id, position, stable_json(spec), stable_json(source), path, image_hash, now, now))
                ids.append(pid); position += 1
            return [public_page(conn, page_row(conn, book_id, pid), False) for pid in ids]
    except BaseException:
        for path, _ in rendered: s.asset_path(path).unlink(missing_ok=True)
        raise


def update_page(book_id, page_id, revision, payload):
    c.integer(revision, 1, 2**31, "revision")
    spec = c.validate(payload)
    with connection(book_id) as conn:
        row = page_row(conn, book_id, page_id)
        if row["revision"] != revision:
            raise MangaError("此页已在另一窗口修改，请刷新后重试", "page_conflict", 409)
        captured = sources(conn, book_id, spec)
    path, image_hash = render(spec, captured)
    try:
        with connection(book_id, True) as conn:
            row = page_row(conn, book_id, page_id)
            if row["revision"] != revision:
                raise MangaError("此页已在另一窗口修改，请刷新后重试", "page_conflict", 409)
            if sources(conn, book_id, spec) != captured:
                raise MangaError("拼页期间分镜已变化，请重试", "manga_page_stale", 409)
            conn.execute("UPDATE manga_pages SET spec_json=?,sources_json=?,image_path=?,image_hash=?,revision=revision+1,updated_at=? WHERE id=?",
                         (stable_json(spec), stable_json(captured), path, image_hash, int(time.time()), page_id))
            return public_page(conn, page_row(conn, book_id, page_id), False)
    except BaseException:
        s.asset_path(path).unlink(missing_ok=True); raise


def auto_pages(book_id, payload):
    if not isinstance(payload, dict) or set(payload) - {"job_ids", "settings"}:
        raise MangaError("自动拼页参数无效")
    ids, options = payload.get("job_ids"), payload.get("settings", {})
    if not isinstance(ids, list) or not 1 <= len(ids) <= 120 or any(not isinstance(i, str) for i in ids) or len(set(ids)) != len(ids):
        raise MangaError("请选择 1–120 张不同分镜")
    if not isinstance(options, dict) or "panels" in options:
        raise MangaError("自动拼页设置无效")
    with connection(book_id) as conn:
        rows = sources(conn, book_id, c.validate({**options, "panels": [{"job_id": ids[0]}]}))
        for pid in ids[1:]: rows += sources(conn, book_id, {"panels": [{"job_id": pid}]})
    # Preserve supplied order within each chapter, never join two chapters on one page.
    grouped = {}
    for row in rows: grouped.setdefault(row["chapter"], []).append(row["job_id"])
    size = c.LAYOUTS.get(options.get("layout"), 4)
    specs = []
    for chapter in sorted(grouped):
        items = grouped[chapter]
        for start in range(0, len(items), size):
            specs.append({**options, "panels": [{"job_id": pid} for pid in items[start:start + size]]})
    return create_pages(book_id, specs)


def edition_links(conn, book_id):
    row = conn.execute("SELECT * FROM manga_exports WHERE book_id=? OR comic_book_id=? ORDER BY created_at DESC,rowid DESC LIMIT 1", (book_id, book_id)).fetchone()
    if not row:
        return None
    manifest = json.loads(row["manifest_json"])
    return {"source_book_id": row["book_id"], "comic_book_id": row["comic_book_id"],
            "pages": [{"chapter": p["chapter"], "para": p["para"]} for p in manifest["pages"]]}


def cached_export(conn, book_id, fingerprint, count):
    old = conn.execute("SELECT e.*,b.sha256 FROM manga_exports e JOIN books b ON b.id=e.comic_book_id WHERE e.book_id=? AND e.fingerprint=?", (book_id, fingerprint)).fetchone()
    if old:
        archive = s.asset_path(old["archive_path"])
        if archive.is_file() and file_hash(archive) == old["sha256"]:
            conn.execute("DELETE FROM library_state WHERE book_id=?", (old["comic_book_id"],))
            return export_result(book_id, old["id"], old["comic_book_id"], count)
    return None


def export_book(book_id):
    with connection(book_id) as conn:
        rows = conn.execute("SELECT * FROM manga_pages WHERE book_id=? AND deleted=0 ORDER BY position,id", (book_id,)).fetchall()
        if not rows or len(rows) > 500:
            raise MangaError("导出须包含 1–500 页")
        for row in rows: check_page(conn, row)
        book = dict(conn.execute("SELECT * FROM books WHERE id=?", (book_id,)).fetchone())
        pages = []
        for row in rows:
            spec, src = json.loads(row["spec_json"]), json.loads(row["sources_json"])
            pages.append({"id": row["id"], "revision": row["revision"], "image_hash": row["image_hash"], "image_path": row["image_path"],
                          "chapter": src[0]["chapter"], "para": src[0]["para"], "width": spec["width"], "height": spec["height"]})
        manifest = {"version": 1, "source_book_id": book_id, "title": book["title"] + " · 漫画", "author": book["author"], "pages": pages}
        fingerprint = digest(manifest)
        cached = cached_export(conn, book_id, fingerprint, len(pages))
        if cached:
            return cached
    export_id = s.uid("edition_")
    relative = "exports/" + export_id + ".cbz"
    archive = s.asset_path(relative); archive.parent.mkdir(parents=True, exist_ok=True)
    try:
        comic = ET.Element("ComicInfo")
        for key, value in [("Title", manifest["title"]), ("Writer", book["author"] or ""), ("PageCount", len(pages)), ("Manga", "Yes"), ("Notes", "Velmira edition " + fingerprint)]:
            ET.SubElement(comic, key).text = re.sub(r"[\x00-\x08\x0b\x0c\x0e-\x1f]", "", str(value))
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_STORED) as zf:
            # Fixed timestamps make repairing an edition byte-for-byte reproducible.
            zf.writestr(zipfile.ZipInfo("ComicInfo.xml"), ET.tostring(comic, encoding="utf-8", xml_declaration=True))
            for idx, page in enumerate(pages):
                content_hash = hashlib.sha256()
                with s.asset_path(page["image_path"]).open("rb") as src, zf.open(zipfile.ZipInfo(f"pages/{idx + 1:04d}.png"), "w", force_zip64=True) as dest:
                    while chunk := src.read(1024 * 1024):
                        content_hash.update(chunk); dest.write(chunk)
                if content_hash.hexdigest() != page["image_hash"]:
                    raise MangaError("导出期间漫画页图片已变化，请重试", "page_conflict", 409)
        sha = file_hash(archive)
        with connection(book_id, True) as conn:
            current = conn.execute("SELECT * FROM manga_pages WHERE book_id=? AND deleted=0 ORDER BY position,id", (book_id,)).fetchall()
            if [(r["id"], r["revision"], r["image_hash"]) for r in current] != [(p["id"], p["revision"], p["image_hash"]) for p in pages]:
                raise MangaError("导出期间漫画页已修改或重排，请重新导出", "page_conflict", 409)
            for row in current: check_page(conn, row)
            cached = cached_export(conn, book_id, fingerprint, len(pages))
            if cached:
                archive.unlink(missing_ok=True)
                return cached
            # A previously downloaded CBZ may already be in the library.
            existing = conn.execute("SELECT id FROM books WHERE sha256=?", (sha,)).fetchone()
            comic_id, now = existing["id"] if existing else "comic_" + sha[:20], int(time.time())
            if not existing:
                conn.execute("INSERT INTO books(id,title,author,format,file_path,sha256,chapter_count,created_at) VALUES(?,?,?,'cbz',?,?,?,?)",
                             (comic_id, manifest["title"], book["author"], str(archive), sha, len(pages), now))
                for idx, page in enumerate(pages):
                    conn.execute("INSERT INTO book_pages VALUES(?,?,?,?,?,?)", (comic_id, idx, page["image_path"], page["width"], page["height"], s.asset_path(page["image_path"]).stat().st_size))
            else:
                conn.execute("UPDATE books SET file_path=? WHERE id=?", (str(archive), comic_id))
            conn.execute("DELETE FROM library_state WHERE book_id=?", (comic_id,))
            old = conn.execute("SELECT id FROM manga_exports WHERE book_id=? AND fingerprint=?", (book_id, fingerprint)).fetchone()
            if old:
                export_id = old["id"]
                conn.execute("UPDATE manga_exports SET archive_path=?,comic_book_id=? WHERE id=?", (relative, comic_id, export_id))
            else:
                conn.execute("INSERT INTO manga_exports VALUES(?,?,?,?,?,?,?)", (export_id, book_id, fingerprint, stable_json(manifest), relative, comic_id, now))
        return export_result(book_id, export_id, comic_id, len(pages))
    except BaseException:
        archive.unlink(missing_ok=True); raise


def export_result(book_id, export_id, comic_id, count):
    return {"id": export_id, "comic_book_id": comic_id, "page_count": count,
            "download_url": f"/api/books/{book_id}/manga/exports/{export_id}/download", "read_url": "/?book=" + comic_id}
