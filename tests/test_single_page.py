"""One image call renders a whole text-free comic page."""
import json
import time
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from fastapi.testclient import TestClient
from PIL import Image
import io

from server.app import db
from server.app.ai import settings
from server.app.main import create_app
from server.app.manga import single_page as sp
from manga_fixture import seed


def plan(count=4):
    panels = []
    for index in range(count):
        panels.append({"shot": "wide" if index % 2 == 0 else "reaction",
                       "characters": [],
                       "action": "A lone figure walks through a quiet street, step " + str(index + 1),
                       "environment": "empty street at dusk", "lighting": "soft dusk light",
                       "emotion": "calm"})
    return {"title": "无字测试页", "panels": panels}


def png_bytes():
    buffer = io.BytesIO()
    Image.new("RGB", (64, 96), "#223344").save(buffer, "PNG")
    return buffer.getvalue()


class SinglePageTests(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(); self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for target, value in [("server.app.db.DB_PATH", root / "app.db"), ("server.app.config.DATA_DIR", root)]:
            patcher = patch(target, value); patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch("server.app.ai.retry.RETRY_DELAY", 0); patcher.start(); self.addCleanup(patcher.stop)
        db.init_db(); seed()
        settings.save_settings({"base_url": "https://text.example/v1", "model": "fixture-text",
                                "enabled": True, "auto_analyze": False, "api_key": "test-secret"})
        # Keep the app lifespan/portal loop alive so the background one-shot task runs.
        self.client = TestClient(create_app())
        self.client.__enter__()
        self.addCleanup(lambda: self.client.__exit__(None, None, None))
        self.base = "/api/books/m3-book/manga/single-page"
        response = self.client.patch("/api/books/m3-book/manga/config", json={"provider": "mock", "width": 256, "height": 256})
        self.assertEqual(response.status_code, 200, response.text)

    def wait(self, page_id):
        for _ in range(150):
            response = self.client.get(self.base + "/" + page_id)
            self.assertEqual(response.status_code, 200, response.text)
            page = response.json()["page"]
            if page["status"] in {"done", "error"}:
                return page
            time.sleep(.1)
        self.fail("single page did not finish")

    def test_one_text_free_page_and_one_image_call(self):
        class Provider:
            def __init__(self):
                self.calls = 0
            async def complete(self, **kwargs):
                self.calls += 1
                return plan(4)
        provider = Provider(); images = []

        def fake_image(seed_value, width, height):
            images.append((seed_value, width, height))
            return png_bytes()

        with patch.object(sp, "make_provider", return_value=provider), \
             patch.object(sp, "mock_image", side_effect=fake_image):
            response = self.client.post(self.base, json={"chapter": 0, "panels": 4, "size": "portrait"})
            self.assertEqual(response.status_code, 202, response.text)
            page = self.wait(response.json()["page"]["id"])
        self.assertEqual(page["status"], "done", page)
        self.assertEqual(len(page["spec"]["panels"]), 4)
        self.assertEqual(provider.calls, 1)
        self.assertEqual(len(images), 1)
        self.assertEqual(images[0][1:], (1024, 1536))
        image = self.client.get(page["url"])
        self.assertEqual(image.status_code, 200)
        self.assertEqual(image.headers["content-type"], "image/png")
        conn = db.get_conn()
        try:
            row = conn.execute("SELECT prompt FROM single_comic_pages WHERE id=?", (page["id"],)).fetchone()
        finally:
            conn.close()
        prompt = row["prompt"]
        self.assertIn("exactly 4 panels", prompt)
        self.assertIn("Panel 4", prompt)
        self.assertIn("absolutely no text", prompt)
        self.assertIn("no speech bubbles", prompt)
        self.assertNotIn("無字", prompt)

    def test_scene_uses_accepted_reference_and_character_identity(self):
        from server.app.manga import service as s
        relative = s.save_image(png_bytes(), "ref_test")
        conn = db.get_conn()
        try:
            form = conn.execute("SELECT * FROM character_forms WHERE id='form_m3-char'").fetchone()
            with conn:
                conn.execute("INSERT OR REPLACE INTO form_refs(id,form_id,form_version,identity_hash,path,accepted,created_at) VALUES(?,?,?,?,?,?,?)",
                             ("ref_test", form["id"], form["version"], form["identity_hash"], relative, 1, 1))
        finally:
            conn.close()
        class Provider:
            async def complete(self, **kwargs):
                return plan(3) | {"panels": [
                    {"shot": "wide", "characters": ["m3-char"], "action": "The adult stands in a quiet park",
                     "environment": "park", "lighting": "daylight", "emotion": "calm"}] * 3}
        captured = {}
        def fake_render(cfg, prompt, reference, request_id):
            captured.update(prompt=prompt, reference=reference)
            return png_bytes()
        with patch.object(sp, "make_provider", return_value=Provider()), \
             patch.object(sp, "_render", side_effect=fake_render):
            response = self.client.post(self.base, json={"scene_id": "m3-scene", "panels": 3, "size": "tall"})
            self.assertEqual(response.status_code, 202, response.text)
            page = self.wait(response.json()["page"]["id"])
        self.assertEqual(page["status"], "done", page)
        self.assertEqual(captured["reference"], relative)
        self.assertIn("Character identity references", captured["prompt"])
        self.assertIn("Reference adult", captured["prompt"])
        self.assertIn("Use the supplied character reference", captured["prompt"])

    def test_rejects_unsupported_panel_counts_and_size(self):
        for payload in [{"chapter": 0, "panels": 1, "size": "portrait"},
                        {"chapter": 0, "panels": 7, "size": "portrait"},
                        {"chapter": 0, "panels": 4, "size": "huge"},
                        {"chapter": 0, "panels": "4", "size": "portrait"},
                        {"panels": 4, "size": "portrait"}]:
            response = self.client.post(self.base, json=payload)
            self.assertEqual(response.status_code, 400, response.text)

    def test_unconfigured_text_model_fails_before_queue(self):
        settings.save_settings({"enabled": False})
        response = self.client.post(self.base, json={"chapter": 0, "panels": 4, "size": "portrait"})
        self.assertEqual(response.status_code, 409, response.text)


if __name__ == "__main__":
    unittest.main()
