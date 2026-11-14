"""EPUB illustration retention stays inside the book and ignores active content."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from fastapi.testclient import TestClient

from server.app import db, importer
from server.app.main import create_app


class EpubAssetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for target, value in [("server.app.config.DATA_DIR", self.root), ("server.app.db.DB_PATH", self.root / "app.db")]:
            mocked = patch(target, value); mocked.start(); self.addCleanup(mocked.stop)
        db.init_db()

    def epub(self, image=b"PNG-DATA", extra=""):
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            archive.writestr("mimetype", "application/epub+zip")
            archive.writestr("META-INF/container.xml", "<container><rootfiles><rootfile full-path='OEBPS/book.opf'/></rootfiles></container>")
            archive.writestr("OEBPS/book.opf", """<package xmlns='urn:oasis:names:tc:opendocument:xmlns:container'><metadata><title>Illustrated</title></metadata><manifest>
              <item id='chapter' href='chapter.xhtml' media-type='application/xhtml+xml'/>
              <item id='pic' href='images/pic.png' media-type='image/png'/>
              <item id='remote' href='https://example.com/remote.png' media-type='image/png'/>
            </manifest><spine><itemref idref='chapter'/></spine></package>""")
            archive.writestr("OEBPS/chapter.xhtml", "<html><body><h1>第一章</h1><p>正文<strong>重点</strong>。</p><img src='images/pic.png'><img src='https://example.com/x.png'><script>alert(1)</script>" + extra + "</body></html>")
            archive.writestr("OEBPS/images/pic.png", image)
        return output.getvalue()

    def test_referenced_raster_is_saved_and_served_with_book_scope(self):
        result = importer.import_book(self.epub(), "illustrated.epub")
        book = result["book"]
        self.assertEqual(result["paragraphs"], 1)
        conn = db.get_conn()
        assets = [dict(row) for row in conn.execute("SELECT * FROM epub_assets WHERE book_id=?", (book["id"],))]
        conn.close()
        self.assertEqual(len(assets), 1)
        self.assertEqual(assets[0]["path"], "OEBPS/images/pic.png")
        with TestClient(create_app()) as client:
            chapter = client.get(f"/api/books/{book['id']}/chapters/0")
            self.assertEqual(chapter.status_code, 200, chapter.text)
            payload = chapter.json()
            self.assertEqual(len(payload["illustrations"]), 1)
            self.assertEqual(payload["paragraph_formats"], [{"para": 0, "formats": [{"kind": "strong", "start": 2, "end": 4}]}])
            url = payload["illustrations"][0]["url"]
            image = client.get(url)
            self.assertEqual(image.status_code, 200)
            self.assertEqual(image.content, b"PNG-DATA")
            self.assertIn("nosniff", image.headers["x-content-type-options"])
            self.assertEqual(client.get(f"/api/books/other/epub/assets/{assets[0]['id']}").status_code, 404)

    def test_dangerous_or_oversized_resources_are_ignored_without_html_storage(self):
        with patch("server.app.importer_epub.MAX_EPUB_IMAGE_BYTES", 4):
            result = importer.import_book(self.epub(image=b"12345", extra="<img src='data:image/png;base64,AAAA'>"), "unsafe.epub")
        book = result["book"]
        conn = db.get_conn()
        assets = conn.execute("SELECT COUNT(*) FROM epub_assets WHERE book_id=?", (book["id"],)).fetchone()[0]
        paragraph = conn.execute("SELECT paras_json FROM chapters WHERE book_id=?", (book["id"],)).fetchone()[0]
        conn.close()
        self.assertEqual(assets, 0)
        self.assertNotIn("script", paragraph.lower())
        self.assertNotIn("data:", paragraph.lower())
        self.assertNotIn("<", paragraph)

    def test_repeated_and_image_only_spine_items_keep_occurrences_and_order(self):
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            archive.writestr("mimetype", "application/epub+zip")
            archive.writestr("META-INF/container.xml", "<container><rootfiles><rootfile full-path='OEBPS/book.opf'/></rootfiles></container>")
            archive.writestr("OEBPS/book.opf", """<package><metadata><title>Order</title></metadata><manifest>
              <item id='one' href='one.xhtml' media-type='application/xhtml+xml'/><item id='only' href='only.xhtml' media-type='application/xhtml+xml'/>
              <item id='pic' href='pic.png' media-type='image/png'/></manifest><spine><itemref idref='one'/><itemref idref='only'/></spine></package>""")
            archive.writestr("OEBPS/one.xhtml", "<html><body><h1>一</h1><p>上文</p><img src='pic.png'><p>下文</p><img src='pic.png'></body></html>")
            archive.writestr("OEBPS/only.xhtml", "<html><body><img src='pic.png' alt='封面内图'></body></html>")
            archive.writestr("OEBPS/pic.png", b"PNG-DATA")
        result = importer.import_book(output.getvalue(), "order.epub")
        book_id = result["book"]["id"]
        conn = db.get_conn()
        occurrences = [dict(row) for row in conn.execute("SELECT chapter_idx,before_para,ordinal,alt FROM epub_illustrations WHERE book_id=? ORDER BY chapter_idx,ordinal", (book_id,))]
        assets = conn.execute("SELECT COUNT(*) FROM epub_assets WHERE book_id=?", (book_id,)).fetchone()[0]
        conn.close()
        self.assertEqual(assets, 1)
        self.assertEqual([(row["chapter_idx"], row["before_para"], row["ordinal"]) for row in occurrences], [(0, 1, 0), (0, 2, 1), (1, 0, 0)])
        self.assertEqual(occurrences[-1]["alt"], "封面内图")
        with TestClient(create_app()) as client:
            first = client.get(f"/api/books/{book_id}/chapters/0").json()
            second = client.get(f"/api/books/{book_id}/chapters/1").json()
        self.assertEqual([item["before_para"] for item in first["illustrations"]], [1, 2])
        self.assertEqual(len(second["paragraphs"]), 0)
        self.assertEqual(second["illustrations"][0]["before_para"], 0)


if __name__ == "__main__":
    unittest.main()
