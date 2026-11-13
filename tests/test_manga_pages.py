"""M4 composition, versioning and portable CBZ checks with disposable data."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile
from xml.etree import ElementTree as ET

from fastapi.testclient import TestClient
from PIL import Image

from server.app import db, importer
from server.app.main import create_app
from server.app.manga import composition as c, pages as p, queue
from server.app.manga.models import MangaError
from manga_fixture import seed


class PageTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.config.DATA_DIR", self.root)]:
            mock = patch(target, value); mock.start(); self.addCleanup(mock.stop)
        db.init_db(); seed(); queue._stop.clear()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.base = "/api/books/m3-book/manga"
        self.call("/config", {"provider": "mock", "width": 256, "height": 256}, "PATCH")
        image = io.BytesIO(); Image.new("RGB", (64, 64), "red").save(image, "PNG")
        ref = self.client.post(self.base + "/forms/form_m3-char/refs", content=image.getvalue()).json()["ref"]
        self.ref = ref["id"]
        self.call("/refs/" + self.ref, {"accepted": True}, "PATCH")
        self.job = self.panel()
        self.spec = {"width": 640, "height": 960, "panels": [{"job_id": self.job, "caption": "雨停了。", "dialogue": "我们出发吧！", "speaker": "林澄"}]}

    def sql(self, query, params=()):
        conn = db.get_conn()
        try:
            with conn: return [dict(r) for r in conn.execute(query, params).fetchall()]
        finally: conn.close()

    def call(self, path, data=None, method="POST", status=200):
        response = self.client.request(method, self.base + path, json=data)
        self.assertEqual(response.status_code, status, response.text)
        return response.json()

    def panel(self, scene="m3-scene", approve=True):
        job = self.call("/jobs", {"scene_id": scene, "character_id": "m3-char"}, status=201)["job"]
        self.sql("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        queue.process(job["id"])
        if approve: self.call("/jobs/" + job["id"] + "/approve")
        return job["id"]

    def create(self, spec=None):
        return self.call("/pages", spec or self.spec, status=201)["page"]

    def test_seven_layouts_geometry_rounding_and_chinese(self):
        source = self.root / "art.png"; Image.new("RGB", (512, 768), "red").save(source)
        for layout, count in c.LAYOUTS.items():
            with self.subTest(layout=layout):
                spec = c.validate({"layout": layout, "title": "花园的钥匙", "panels": [{"job_id": self.job, "caption": "细雨过后，花园醒来了。", "dialogue": "找到了！", "bubble": "ellipse" if i % 2 else "rounded", "fit": "cover"} for i in range(count)]})
                boxes = c.rectangles(spec)
                self.assertEqual(len(boxes), count)
                for i, (x, y, r, b) in enumerate(boxes):
                    self.assertTrue(0 <= x < r <= spec["width"] and 0 <= y < b <= spec["height"])
                    for x2, y2, r2, b2 in boxes[i + 1:]:
                        self.assertTrue(r <= x2 or r2 <= x or b <= y2 or b2 <= y)
                dest = self.root / (layout + ".png"); c.compose(spec, [source] * count, dest)
                with Image.open(dest) as img:
                    self.assertEqual(img.size, (1200, 1800))
                    self.assertEqual(img.getpixel(boxes[0][:2]), (255, 255, 255))
                self.assertEqual(len(c.wrap("雨停了。\n出发！", c.font("sans", 32), 200)), 2)

    def test_validation_and_overflow_never_silently_truncate(self):
        for changes in [{"width": True}, {"font": []}, {"layout": {}}, {"title": 42}, {"panels": []}, {"unknown": 1}, {"panels": [{"job_id": self.job, "caption": "a" * 301}]}]:
            self.call("/pages", {**self.spec, **changes}, status=400)
        overflow = {"width": 640, "height": 960, "layout": "3x2", "panels": [{"job_id": self.job, "caption": "汉" * 300, "dialogue": "字" * 400}]}
        self.call("/pages", overflow, status=409)
        self.assertEqual(self.call("/pages", method="GET")["pages"], [])

    def test_reject_unreviewed_cross_book_reference_and_stale_images(self):
        pending = self.panel(approve=False)
        self.call("/pages", {"panels": [{"job_id": pending}]}, status=409)
        self.sql("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES('other','Other','other.txt','other',1)")
        self.assertEqual(self.client.post("/api/books/other/manga/pages", json=self.spec).status_code, 404)
        self.sql("UPDATE render_jobs SET kind='reference' WHERE id=?", (self.job,))
        self.call("/pages", self.spec, status=404)
        self.sql("UPDATE render_jobs SET kind='panel' WHERE id=?", (self.job,))
        page = self.create()
        self.call("/refs/" + self.ref, {"accepted": False}, "PATCH")
        self.call("/exports", status=409)
        self.assertEqual(self.call("/pages", method="GET")["pages"][0]["status"], "stale")
        self.call("/refs/" + self.ref, {"accepted": True}, "PATCH")
        path = p.s.asset_path(self.sql("SELECT image_path FROM render_jobs WHERE id=?", (self.job,))[0]["image_path"])
        Image.new("RGB", (32, 32), "blue").save(path)
        self.call("/exports", status=409)
        page_image = p.s.asset_path(self.sql("SELECT image_path FROM manga_pages WHERE id=?", (page["id"],))[0]["image_path"])
        page_image.unlink()
        self.call("/exports", status=409)

    def test_revision_order_delete_restore_and_immutable_editions(self):
        first, second = self.create(), self.create({**self.spec, "title": "第二页"})
        edition = self.call("/exports", status=201)["export"]
        original = self.client.get(edition["download_url"]).content
        updated = self.call("/pages/" + first["id"], {"revision": 1, "spec": {**self.spec, "title": "改字"}}, "PATCH")["page"]
        self.call("/pages/" + first["id"], {"revision": 1, "spec": self.spec}, "PATCH", 409)
        order = [second["id"], first["id"]]
        stale = {"order": order, "revisions": {first["id"]: 1, second["id"]: 1}}
        self.call("/pages/order", stale, "PUT", 409)
        stale["revisions"][first["id"]] = updated["revision"]
        self.call("/pages/order", stale, "PUT")
        self.call("/pages/" + second["id"] + "?revision=2", method="DELETE")
        removed = self.call("/pages?deleted=true", method="GET")["pages"][0]
        self.call("/pages/" + second["id"] + "/restore", {"revision": removed["revision"]})
        self.assertEqual(self.call("/pages", method="GET")["pages"][-1]["id"], second["id"])
        new = self.call("/exports", status=201)["export"]
        self.assertNotEqual(edition["comic_book_id"], new["comic_book_id"])
        self.assertEqual(self.client.get(edition["download_url"]).content, original)
        self.assertEqual(self.call("/exports", status=201)["export"], new)
        self.assertEqual(self.client.get(first["url"]).status_code, 409)

    def test_missing_or_corrupt_archive_is_repaired_without_duplicate_book(self):
        self.create(); first = self.call("/exports", status=201)["export"]
        original = self.client.get(first["download_url"]).content
        for corrupt in (False, True):
            archive = p.s.asset_path(self.sql("SELECT archive_path FROM manga_exports")[0]["archive_path"])
            if corrupt: archive.write_bytes(b"corrupt")
            else: archive.unlink()
            self.assertEqual(self.call("/exports", status=201)["export"], first)
            self.assertEqual(self.client.get(first["download_url"]).content, original)
            self.assertEqual(len(self.sql("SELECT id FROM books WHERE format='cbz'")), 1)

    def test_noop_revision_and_concurrent_exports_are_idempotent(self):
        from concurrent.futures import ThreadPoolExecutor
        first = self.create()
        with ThreadPoolExecutor(max_workers=2) as pool:
            editions = list(pool.map(lambda _: p.export_book("m3-book"), range(2)))
        self.assertEqual(editions[0], editions[1])
        self.call("/pages/" + first["id"], {"revision": 1, "spec": self.spec}, "PATCH")
        revised = self.call("/exports", status=201)["export"]
        self.assertNotEqual(editions[0]["id"], revised["id"])
        self.assertEqual(len(self.sql("SELECT id FROM manga_exports")), 2)

    def test_sources_changing_during_render_roll_back_batch(self):
        original = p.render
        def change(spec, sources):
            result = original(spec, sources)
            self.sql("UPDATE form_refs SET accepted=0 WHERE id=?", (self.ref,))
            return result
        with patch.object(p, "render", side_effect=change):
            self.call("/pages", self.spec, status=409)
        self.assertEqual(self.sql("SELECT id FROM manga_pages"), [])
        self.assertEqual(list((self.root / "assets/pages").glob("*.png")), [])

    def test_pages_changing_during_export_roll_back(self):
        self.create(); original = zipfile.ZipFile.writestr
        def change(archive, *args, **kwargs):
            self.sql("UPDATE manga_pages SET revision=revision+1")
            return original(archive, *args, **kwargs)
        with patch.object(zipfile.ZipFile, "writestr", new=change):
            self.call("/exports", status=409)
        self.assertEqual(self.sql("SELECT id FROM manga_exports"), [])
        self.assertEqual(list((self.root / "exports").glob("*.cbz")), [])

    def test_twenty_pages_five_chapters_cbz_import_and_reading_links(self):
        ids = []
        for chapter in range(5):
            plan = {"summary": "a quiet garden", "characters": [{}], "character_ids": ["m3-char"]}
            scene = "page-scene-" + str(chapter)
            self.sql("INSERT INTO segments(id,book_id,chapter,idx,strategy,params_hash,start_para,end_para,text,features_json,score) VALUES(?,?,?,0,'chapter','m4',0,0,'sample','{}',1)", ("seg-" + str(chapter), "m3-book", chapter))
            self.sql("INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) VALUES(?,'m3-book',?,?,?,1,1)", (scene, chapter, "seg-" + str(chapter), json.dumps(plan)))
            ids.extend(self.panel(scene) for _ in range(4))
        pages = self.call("/pages/auto", {"job_ids": ids, "settings": {"layout": "1x1", "width": 640, "height": 960}}, status=201)["pages"]
        self.assertEqual(len(pages), 20)
        edition = self.call("/exports", status=201)["export"]
        raw = self.client.get(edition["download_url"]).content
        with zipfile.ZipFile(io.BytesIO(raw)) as archive:
            self.assertEqual(archive.namelist(), ["ComicInfo.xml"] + [f"pages/{i:04d}.png" for i in range(1, 21)])
            self.assertEqual(ET.fromstring(archive.read("ComicInfo.xml")).findtext("PageCount"), "20")
            self.assertNotIn(b"snapshot", archive.read("ComicInfo.xml"))
            for name in archive.namelist()[1:]:
                with Image.open(io.BytesIO(archive.read(name))) as image: self.assertEqual(image.size, (640, 960))
        links = self.client.get("/api/books/m3-book").json()["book"]["reading_links"]
        self.assertEqual([p["chapter"] for p in links["pages"]], [c for c in range(5) for _ in range(4)])
        reverse = self.client.get("/api/books/" + edition["comic_book_id"]).json()["book"]["reading_links"]
        self.assertEqual(reverse, links)
        media = self.client.get("/api/books/" + edition["comic_book_id"] + "/pages?limit=50").json()
        self.assertEqual(len(media["pages"]), 20)
        # A fresh database proves the CBZ is portable, not dependent on source snapshots.
        fresh = self.root / "fresh"; fresh.mkdir()
        with patch("server.app.db.DB_PATH", fresh / "app.db"), patch("server.app.config.DATA_DIR", fresh):
            db.init_db(); imported = importer.import_book(raw, "sample.cbz")
            self.assertEqual(imported["book"]["chapter_count"], 20)

    def test_auto_pages_keep_chapters_separate(self):
        plan = {"summary": "next chapter", "characters": [{}], "character_ids": ["m3-char"]}
        self.sql("INSERT INTO segments(id,book_id,chapter,idx,strategy,params_hash,start_para,end_para,text,features_json,score) VALUES('next-seg','m3-book',1,0,'chapter','m4',0,0,'sample','{}',1)")
        self.sql("INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) VALUES('next','m3-book',1,'next-seg',?,1,1)", (json.dumps(plan),))
        ids = [self.panel("next"), self.job, self.panel("next")]
        pages = self.call("/pages/auto", {"job_ids": ids}, status=201)["pages"]
        self.assertEqual(len(pages), 2)
        self.assertEqual([p["spec"]["layout"] for p in pages], ["1x1", "2x1"])
        self.assertEqual(pages[1]["spec"]["panels"][0]["job_id"], ids[0])


if __name__ == "__main__": unittest.main()
