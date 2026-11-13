"""书架管理：真实 SQLite、旧库幂等升级、可恢复移出与重新导入。"""
import asyncio
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from starlette.requests import Request
from server.app import db, importer
from server.app.api import books


def request(body):
    async def receive():
        return {"type": "http.request", "body": json.dumps(body).encode(), "more_body": False}
    return Request({"type": "http", "method": "PATCH", "headers": []}, receive)


class LibraryManagementTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        for target, value in [("server.app.db.DB_PATH", root / "app.db"), ("server.app.config.DATA_DIR", root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db()
        self.raw = "第一章 开始\n\n这是测试正文，必须保留阅读数据。\n\n第二段正文。".encode()
        self.book = importer.import_book(self.raw, "测试书籍.txt")["book"]
        self.id = self.book["id"]

    def update(self, body, book_id=None):
        return asyncio.run(books.update_book(book_id or self.id, request(body)))

    def test_rename_validation_and_missing_book(self):
        self.assertEqual(self.update({"title": "  新书名  "})["book"]["title"], "新书名")
        for value in [" ", "a" * 201, 8, None]:
            self.assertEqual(self.update({"title": value}).status_code, 400)
        for body in [{}, [], {"archived": "true"}, {"title": "ok", "unknown": 1}]:
            self.assertEqual(self.update(body).status_code, 400)
        self.assertEqual(self.update({"title": "新书名"}, "missing").status_code, 404)
        self.assertEqual(books.get_book(self.id)["book"]["title"], "新书名")

    def test_archive_restore_preserves_progress_notes_and_m2(self):
        conn = db.get_conn()
        with conn:
            conn.execute("INSERT INTO progress VALUES(?,?,?,?,?,?)", (self.id, 0, 0, 3, .25, 123))
            conn.execute("INSERT INTO annotations VALUES(?,?,?,?,?,?,?,?,?,?,?)", ("a1", self.id, 0, 0, 0, 2, "这是", "批注", "yellow", 1, 1))
        conn.close()
        # M2 引用与正文都必须留在库中。
        conn = db.get_conn()
        with conn:
            conn.execute("INSERT INTO characters(id,book_id,name,created_at,updated_at) VALUES(?,?,?,?,?)", ("c1", self.id, "人物", 1, 1))
        conn.close()
        before = self.snapshot()
        self.update({"archived": True})
        self.update({"archived": True})  # 重试幂等
        self.assertEqual(books.list_books()["books"], [])
        self.assertEqual(books.list_books(True)["books"][0]["id"], self.id)
        db.init_db(); db.init_db()  # 升级/重启不能重新显示已移出的书
        self.assertEqual(books.list_books()["books"], [])
        self.update({"archived": False})
        row = books.list_books()["books"][0]
        self.assertTrue(row["has_progress"])
        self.assertEqual(row["last_read_at"], 123)
        self.assertEqual(books.list_books(True)["books"], [])
        self.assertEqual(self.snapshot(), before)

    def snapshot(self):
        conn = db.get_conn()
        try:
            return {t: [tuple(r) for r in conn.execute(f"SELECT * FROM {t}")]
                    for t in ["books", "chapters", "progress", "annotations", "characters"]}
        finally:
            conn.close()

    def test_reimport_restores_same_book(self):
        self.update({"archived": True})
        async def receive():
            return {"type": "http.request", "body": self.raw, "more_body": False}
        req = Request({"type": "http", "method": "POST", "headers": []}, receive)
        result = asyncio.run(books.create_book(req, filename="测试书籍.txt", force=0))
        data = json.loads(result.body)
        self.assertTrue(data["deduped"])
        self.assertTrue(data["restored"])
        self.assertEqual(data["book"]["id"], self.id)
        self.assertEqual(len(books.list_books()["books"]), 1)

    def test_unread_comic_is_not_inferred_from_first_page_percent(self):
        conn = db.get_conn()
        with conn:
            conn.execute("UPDATE books SET format='cbz', chapter_count=5 WHERE id=?", (self.id,))
        conn.close()
        row = books.list_books()["books"][0]
        self.assertEqual(row["percent"], .2)
        self.assertFalse(row["has_progress"])

    def test_old_database_upgrade_preserves_books(self):
        conn = db.get_conn()
        conn.execute("DROP TABLE library_state"); conn.commit(); conn.close()
        db.init_db()
        self.assertEqual(books.list_books()["books"][0]["id"], self.id)


if __name__ == "__main__":
    unittest.main()
