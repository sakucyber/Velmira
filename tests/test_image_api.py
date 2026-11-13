import base64
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

from fastapi.testclient import TestClient
from PIL import Image

from server.app import db
from server.app.ai import settings
from server.app.main import create_app
from server.app.manga import queue
from server.app.manga.models import DEFAULTS, STYLE_PRESETS, MangaError, validate_config
from server.app.manga.provider import ImageAPIProvider, ComfyProvider
from manga_fixture import seed


class ImageAPITests(unittest.TestCase):
    def setUp(self):
        p = patch("server.app.ai.retry.RETRY_DELAY", 0); p.start(); self.addCleanup(p.stop)
        p = patch("server.app.manga.queue.RETRY_DELAY", 0); p.start(); self.addCleanup(p.stop)
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.config.DATA_DIR", self.root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db(); seed(); queue._stop.clear()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.base = "/api/books/m3-book/manga"
        settings.save_settings({"base_url": "https://text.example/v1", "api_key": "text-secret"})
        settings.save_settings({"base_url": "https://images.example/v1", "api_key": "image-secret"}, "image")
        self.cfg = validate_config({"provider": "openai-images", "base_url": "https://images.example/v1", "model": "image-model"})
        raw = io.BytesIO(); Image.new("RGB", (64, 64), "white").save(raw, "PNG"); self.png = raw.getvalue()

    def test_image_requests_use_correct_endpoint_reference_and_independent_key(self):
        provider = ImageAPIProvider(self.cfg)
        response = json.dumps({"data": [{"b64_json": base64.b64encode(self.png).decode()}]}).encode()
        ref = self.root / "ref.png"; ref.write_bytes(self.png)
        for reference, path in [(None, "/images/generations"), (ref, "/images/edits")]:
            with patch("urllib.request.OpenerDirector.open", return_value=io.BytesIO(response)) as call:
                self.assertEqual(provider.generate({"prompt": "anime garden"}, reference, "request-id"), self.png)
                request = call.call_args.args[0]
                self.assertTrue(request.full_url.endswith(path))
                self.assertEqual(request.get_header("Authorization"), "Bearer image-secret")
                self.assertNotIn(b"text-secret", request.data)
                self.assertEqual(request.get_header("Idempotency-key"), "request-id")
                if reference: self.assertIn(self.png, request.data)

    def test_api_errors_are_sanitized_and_credentials_are_bound_to_destination(self):
        provider = ImageAPIProvider(self.cfg)
        err = HTTPError("https://images.example", 500, "image-secret", {}, io.BytesIO(b"image-secret"))
        with patch("urllib.request.OpenerDirector.open", side_effect=err):
            with self.assertRaises(MangaError) as caught: provider.generate({"prompt": "test"}, None, "request")
            self.assertNotIn("image-secret", str(caught.exception))
        settings.save_settings({"base_url": "https://new.example/v1"}, "image")
        self.assertFalse(settings.public_settings("image")["key_configured"])
        with patch("urllib.request.OpenerDirector.open") as network:
            with self.assertRaises(MangaError): provider.generate({"prompt": "test"}, None, "request")
            network.assert_not_called()
        self.assertNotIn("secret", self.client.get("/api/image-ai/settings").text)

    def test_image_job_uses_api_and_never_silently_marks_identity_passed(self):
        self.assertEqual(self.client.patch(self.base + "/config", json=self.cfg).status_code, 200)
        job = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]
        conn = db.get_conn()
        try:
            with conn: conn.execute("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        finally: conn.close()
        with patch.object(ImageAPIProvider, "generate", return_value=self.png) as generate:
            queue.process(job["id"])
            generate.assert_called_once()
        job = self.client.get(self.base + "/jobs/" + job["id"]).json()["job"]
        self.assertEqual(job["status"], "needs_review")
        self.assertNotIn("secret", json.dumps(job))
        self.assertEqual(job["snapshot"]["config"]["style_preset"], "anime")

    def test_request_retries_three_times_with_one_key_but_restart_does_not_resubmit(self):
        self.client.patch(self.base + "/config", json=self.cfg)
        job = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]
        for attempt in range(2):
            conn = db.get_conn()
            try:
                with conn: conn.execute("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
            finally: conn.close()
            with patch.object(ImageAPIProvider, "generate", side_effect=MangaError("network unavailable")) as network:
                queue.process(job["id"])
                self.assertEqual(network.call_count, 3 if attempt == 0 else 0)
                if attempt == 0: self.assertEqual(len({call.args[2] for call in network.call_args_list}), 1)
        result = self.client.get(self.base + "/jobs/" + job["id"]).json()["job"]
        self.assertEqual(result["status"], "error")

    def test_image_request_recovers_after_two_failures(self):
        self.client.patch(self.base + "/config", json=self.cfg)
        job = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]
        conn = db.get_conn()
        try:
            with conn: conn.execute("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        finally: conn.close()
        with patch.object(ImageAPIProvider, "generate", side_effect=[RuntimeError("private-key"), b"bad image", self.png]) as generate:
            queue.process(job["id"])
        self.assertEqual(generate.call_count, 3)
        self.assertEqual(len({call.args[2] for call in generate.call_args_list}), 1)
        result = self.client.get(self.base + "/jobs/" + job["id"]).json()["job"]
        self.assertEqual(result["status"], "needs_review")
        self.assertNotIn("private-key", json.dumps(result))

    def test_comfy_poll_retries_reuse_submission(self):
        self.client.patch(self.base + "/config", json={"provider": "comfyui"})
        job = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]
        conn = db.get_conn()
        try:
            with conn: conn.execute("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        finally: conn.close()
        with patch.object(ComfyProvider, "probe", return_value={"ok": True}), patch.object(ComfyProvider, "submit", return_value=("same-prompt", {})) as submit, patch.object(ComfyProvider, "result", side_effect=[MangaError("unavailable", "comfy_unavailable"), MangaError("HTTP 503", "comfy_error"), self.png]) as result, patch.object(queue._stop, "wait", return_value=False):
            queue.process(job["id"])
        self.assertEqual(submit.call_count, 1)
        self.assertEqual(result.call_count, 3)
        self.assertEqual({call.args[0] for call in result.call_args_list}, {"same-prompt"})
        self.assertEqual(self.client.get(self.base + "/jobs/" + job["id"]).json()["job"]["status"], "needs_review")

    def test_anime_default_presets_custom_and_backend_validation(self):
        cfg = self.client.get(self.base + "/config").json()
        self.assertEqual(cfg["config"]["style"], STYLE_PRESETS["anime"]["prompt"])
        self.assertEqual(len(cfg["styles"]), 6)
        result = self.client.patch(self.base + "/config", json={"style_preset": "manga"}).json()["config"]
        self.assertEqual(result["style"], STYLE_PRESETS["manga"]["prompt"])
        result = self.client.patch(self.base + "/config", json={"style": "custom style"}).json()["config"]
        self.assertEqual(result["style_preset"], "custom")
        self.assertEqual(self.client.patch(self.base + "/config", json={"style_preset": []}).status_code, 400)
        self.assertEqual(validate_config({"clip": "", "provider": "openai-images", "base_url": "https://images.example/v1", "model": "m"})["provider"], "openai-images")


if __name__ == "__main__": unittest.main()
