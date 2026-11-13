"""Bounded uploads, event-loop responsiveness and archive rollback with disposable data."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import io
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import zipfile

from starlette.requests import Request
from server.app import config, db, importer, importer_cbz
from server.app.api import books


class ImportSafetyTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        for target, value in [("server.app.config.DATA_DIR", self.root), ("server.app.db.DB_PATH", self.root / "app.db")]:
            mock = patch(target, value); mock.start(); self.addCleanup(mock.stop)
        db.init_db()

    def test_content_length_rejected_before_reading_body(self):
        async def receive():
            self.fail("oversized body must not be read")
        request = Request({"type": "http", "headers": [(b"content-length", str(config.MAX_UPLOAD_BYTES + 1).encode())]}, receive)
        response = asyncio.run(books.create_book(request, "large.txt", 0))
        self.assertEqual(response.status_code, 413)
        self.assertEqual(json.loads(response.body)["error"]["code"], "file_too_large")

    def test_source_download_and_private_resources(self):
        from fastapi.testclient import TestClient
        from server.app.main import create_app
        raw = "第一章 测试\n\n可下载的原文件。".encode()
        book = importer.import_book(raw, "book.txt")["book"]
        with TestClient(create_app()) as client:
            url = f"/api/books/{book['id']}/source"
            response = client.get(url)
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response.content, raw)
            self.assertTrue(response.headers["content-disposition"].startswith("attachment;"))
            self.assertEqual(client.get("/api/books/missing/source").status_code, 404)
            self.assertEqual(client.get("/MEMORY.md").status_code, 404)
            self.assertNotIn("data_dir", client.get("/api/health").json())
            # An invalid stored path must not become an arbitrary-file download.
            conn = db.get_conn()
            with conn:
                conn.execute("UPDATE books SET file_path=? WHERE id=?", (str(Path(__file__).resolve()), book["id"]))
            conn.close()
            self.assertEqual(client.get(url).status_code, 404)

    def test_stream_limit_stops_at_first_excess_even_with_false_length(self):
        for headers in ([], [(b"content-length", b"1")]):
            received = []
            async def receive():
                received.append(1)
                self.assertLessEqual(len(received), 2)
                return {"type": "http.request", "body": b"12345", "more_body": len(received) == 1}
            request = Request({"type": "http", "headers": headers}, receive)
            with patch.object(config, "MAX_UPLOAD_BYTES", 8), patch.object(importer, "import_book") as importing:
                result = asyncio.run(books.create_book(request, "large.txt", 0))
            self.assertEqual(result.status_code, 413)
            self.assertEqual(len(received), 2)
            importing.assert_not_called()

    def test_import_does_not_block_loop(self):
        started, release = threading.Event(), threading.Event()
        main_thread = threading.get_ident()
        original = importer.import_book
        def slow_import(*args, **kwargs):
            self.assertNotEqual(threading.get_ident(), main_thread)
            started.set()
            self.assertTrue(release.wait(3), "event loop could not release importer")
            return original(*args, **kwargs)
        async def run():
            async def receive():
                return {"type": "http.request", "body": "第一章 测试\n\n正文内容。".encode(), "more_body": False}
            request = Request({"type": "http", "headers": []}, receive)
            task = asyncio.create_task(books.create_book(request, "book.txt", 0))
            try:
                for _ in range(200):
                    if started.is_set(): break
                    await asyncio.sleep(.005)
                self.assertTrue(started.is_set())
            finally:
                release.set()
            self.assertEqual((await task).status_code, 201)
        with patch.object(importer, "import_book", side_effect=slow_import):
            asyncio.run(run())

    def test_parallel_duplicate_uploads_keep_one_book_and_source(self):
        raw = "第一章 测试\n\n并发上传正文。".encode()
        with ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(lambda _: importer.import_book(raw, "same.txt"), range(8)))
        self.assertEqual(len({r["book"]["id"] for r in results}), 1)
        self.assertEqual(sum(not r["deduped"] for r in results), 1)
        self.assertEqual(len(list((self.root / "books").iterdir())), 1)

    def archive(self, entries):
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
            for name, data in entries: zf.writestr(name, data)
        return out.getvalue()

    def test_archive_count_single_and_total_limits_reject_before_writes(self):
        for setting, limit, entries in [
            ("MAX_CBZ_PAGES", 1, [("1.png", b"a"), ("2.png", b"b")]),
            ("MAX_CBZ_PAGE_BYTES", 10, [("1.png", b"a" * 11)]),
            ("MAX_CBZ_TOTAL_BYTES", 10, [("1.png", b"a" * 6), ("2.png", b"b" * 6)]),
        ]:
            with self.subTest(setting=setting), patch.object(config, setting, limit):
                with self.assertRaises(importer.BookImportError) as caught:
                    importer_cbz.extract_pages(self.archive(entries), "b_test")
                self.assertEqual(caught.exception.status_code, 413)
                self.assertFalse((self.root / "media" / "b_test").exists())

    def test_actual_decompressed_bytes_are_bounded(self):
        info = zipfile.ZipInfo("1.png"); info.file_size = 1
        for setting, limit in [("MAX_CBZ_PAGE_BYTES", 10), ("MAX_CBZ_TOTAL_BYTES", 10)]:
            with self.subTest(setting=setting), patch.object(config, setting, limit):
                with zipfile.ZipFile(io.BytesIO(self.archive([]))) as zf:
                    with patch.object(zf, "open", return_value=io.BytesIO(b"a" * 11)):
                        with self.assertRaises(importer.BookImportError):
                            importer_cbz._read_image_limited(zf, info, 0)

    def test_failed_force_import_preserves_existing_pages(self):
        raw = self.archive([("1.png", b"first"), ("2.png", b"second")])
        media = self.root / "media" / "book"
        media.mkdir(parents=True); (media / "0.png").write_bytes(b"existing")
        original = importer_cbz._read_image_limited
        def corrupt(zf, info, total):
            if info.filename == "2.png": raise zipfile.BadZipFile("bad CRC")
            return original(zf, info, total)
        with patch.object(importer_cbz, "_read_image_limited", side_effect=corrupt):
            with self.assertRaises(zipfile.BadZipFile): importer_cbz.extract_pages(raw, "book", reset=True)
        self.assertEqual((media / "0.png").read_bytes(), b"existing")
        self.assertEqual(list((self.root / "media").iterdir()), [media])

    def test_archive_natural_sort_and_replacement(self):
        raw = self.archive([("10.png", b"ten"), ("2.png", b"two"), ("1.png", b"one")])
        pages = importer_cbz.extract_pages(raw, "book")
        self.assertEqual([p["bytes"] for p in pages], [3, 3, 3])
        self.assertEqual((self.root / pages[1]["path"]).read_bytes(), b"two")
        importer_cbz.extract_pages(self.archive([("1.png", b"new")]), "book", reset=True)
        self.assertEqual([p.name for p in (self.root / "media" / "book").iterdir()], ["0.png"])

