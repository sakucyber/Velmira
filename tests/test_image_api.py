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
from server.app.manga.models import DEFAULTS, STYLE_PRESETS, GENERAL_NEGATIVE, ANIME_EXCLUSIONS, MangaError, validate_config, build_workflow
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

    def test_two_references_keep_order_in_both_supported_protocols(self):
        response = json.dumps({'data':[{'b64_json':base64.b64encode(self.png).decode()}]}).encode()
        paths = [self.root/'first.png',self.root/'second.png']
        paths[0].write_bytes(self.png); paths[1].write_bytes(b'second-reference')
        for mode in ['edits','generations']:
            provider=ImageAPIProvider({**self.cfg,'reference_api':mode})
            with patch('urllib.request.OpenerDirector.open',return_value=io.BytesIO(response)) as call:
                provider.generate({'prompt':'two distinct people'},paths,'two-images')
            req=call.call_args.args[0]
            self.assertTrue(req.full_url.endswith('/images/'+mode))
            if mode=='generations':
                body=json.loads(req.data)
                self.assertEqual([base64.b64decode(x.split(',')[1]) for x in body['image']],[self.png,b'second-reference'])
            else:
                self.assertEqual(req.data.count(b'name="image[]"'),2)
                self.assertLess(req.data.index(self.png),req.data.index(b'second-reference'))

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

    def test_final_image_wire_prompt_carries_anime_and_negative_constraints(self):
        self.client.patch(self.base + "/config", json=self.cfg)
        snapshot = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]["snapshot"]
        response = json.dumps({"data": [{"b64_json": base64.b64encode(self.png).decode()}]}).encode()
        provider = ImageAPIProvider(snapshot["config"])
        with patch("urllib.request.OpenerDirector.open", return_value=io.BytesIO(response)) as call:
            provider.generate(snapshot, None, "anime-wire")
        body = json.loads(call.call_args.args[0].data)
        self.assertIn("full-color 2D manga", body["prompt"])
        self.assertIn("cel shading", body["prompt"])
        self.assertIn("Visual exclusions:", body["prompt"])
        self.assertIn("photorealistic", body["prompt"])
        self.assertIn("speech bubbles", body["prompt"])
        graph = build_workflow(snapshot["config"], snapshot["prompt"], 42)
        self.assertEqual(graph["5"]["inputs"]["text"], snapshot["config"]["negative"])
        self.assertIn("photorealistic", graph["5"]["inputs"]["text"])

    def test_custom_wire_prompt_keeps_explicit_style_and_exclusions(self):
        custom = {**self.cfg, "style": "traditional watercolor on textured paper", "style_preset": "custom", "negative": "watermark, signature"}
        self.client.patch(self.base + "/config", json=custom)
        snapshot = self.client.post(self.base + "/jobs", json={"kind": "reference", "form_id": "form_m3-char"}).json()["job"]["snapshot"]
        response = json.dumps({"data": [{"b64_json": base64.b64encode(self.png).decode()}]}).encode()
        with patch("urllib.request.OpenerDirector.open", return_value=io.BytesIO(response)) as call:
            ImageAPIProvider(snapshot["config"]).generate(snapshot, None, "custom-wire")
        body = json.loads(call.call_args.args[0].data)
        self.assertIn(custom["style"], body["prompt"])
        self.assertNotIn("strictly 2D", body["prompt"])
        self.assertNotIn("photorealistic", body["prompt"])
        self.assertEqual(snapshot["config"]["negative"], custom["negative"])

    def test_old_builtin_defaults_migrate_but_explicit_custom_and_semi_realistic_remain(self):
        old_anime = "Japanese anime illustration, expressive anime eyes, refined cel shading, clean line art, rich colors, cinematic composition"
        old_negative = "nsfw, worst quality, bad anatomy, watermark, text, speech bubbles, extra fingers, duplicate person, crowd, background people, additional faces"
        cases = [
            ({"style": STYLE_PRESETS["semi_realistic"]["prompt"], "negative": old_negative}, "anime", DEFAULTS["style"]),
            ({"style_preset": "anime", "style": old_anime, "negative": old_negative}, "anime", DEFAULTS["style"]),
            ({"style": "bespoke paint style", "negative": "a custom exclusion"}, "custom", "bespoke paint style"),
            ({"style_preset": "semi_realistic", "style": STYLE_PRESETS["semi_realistic"]["prompt"]}, "semi_realistic", STYLE_PRESETS["semi_realistic"]["prompt"]),
        ]
        for saved, preset, style in cases:
            conn = db.get_conn()
            try:
                with conn:
                    conn.execute("INSERT INTO render_configs VALUES('m3-book',?) ON CONFLICT(book_id) DO UPDATE SET config_json=excluded.config_json", (json.dumps({"provider": "mock", **saved}),))
            finally: conn.close()
            cfg = self.client.get(self.base + "/config").json()["config"]
            self.assertEqual(cfg["style_preset"], preset)
            self.assertEqual(cfg["style"], style)
            if preset == "anime": self.assertIn("photorealistic", cfg["negative"])
            if preset == "custom": self.assertEqual(cfg["negative"], "a custom exclusion")

    def test_style_changes_only_translate_builtin_negative_defaults(self):
        anime = validate_config({"provider": "mock"})
        for style in ("cinematic", "manga", "semi_realistic", "watercolor", "chinese", "custom"):
            with self.subTest(style=style):
                change = {"style_preset": style}
                if style == "custom": change["style"] = "custom oil painting"
                cfg = validate_config(change, anime)
                self.assertEqual(cfg["negative"], GENERAL_NEGATIVE)
                self.assertNotIn(ANIME_EXCLUSIONS, cfg["negative"])
                restored = validate_config({"style_preset": "anime"}, cfg)
                self.assertEqual(restored["negative"], DEFAULTS["negative"])
                custom = {**anime, "negative": "watermark, a bespoke exclusion"}
                self.assertEqual(validate_config(change, custom)["negative"], custom["negative"])
                explicit = validate_config({**change, "negative": "blurry, chosen explicitly"}, anime)
                self.assertEqual(explicit["negative"], "blurry, chosen explicitly")
        custom_style = validate_config({"style": "hand-painted oils"}, anime)
        self.assertEqual(custom_style["style_preset"], "custom")
        self.assertEqual(custom_style["negative"], GENERAL_NEGATIVE)
        unchanged = validate_config({"width": 1024}, anime)
        self.assertEqual(unchanged["negative"], DEFAULTS["negative"])

    def test_partial_saved_configs_choose_style_and_negative_consistently(self):
        old_anime = "Japanese anime illustration, expressive anime eyes, refined cel shading, clean line art, rich colors, cinematic composition"
        cases = [
            ({}, "anime", DEFAULTS["style"], DEFAULTS["negative"]),
            ({"style": ""}, "anime", DEFAULTS["style"], DEFAULTS["negative"]),
            ({"style": old_anime}, "anime", DEFAULTS["style"], DEFAULTS["negative"]),
            ({"style_preset": "anime", "style": old_anime}, "anime", DEFAULTS["style"], DEFAULTS["negative"]),
            ({"style_preset": "cinematic"}, "cinematic", STYLE_PRESETS["cinematic"]["prompt"], GENERAL_NEGATIVE),
            ({"style_preset": "cinematic", "style": ""}, "cinematic", STYLE_PRESETS["cinematic"]["prompt"], GENERAL_NEGATIVE),
            ({"style_preset": "cinematic", "style": STYLE_PRESETS["cinematic"]["prompt"], "negative": DEFAULTS["negative"]}, "cinematic", STYLE_PRESETS["cinematic"]["prompt"], GENERAL_NEGATIVE),
            ({"style": STYLE_PRESETS["cinematic"]["prompt"]}, "cinematic", STYLE_PRESETS["cinematic"]["prompt"], GENERAL_NEGATIVE),
            ({"style_preset": "custom", "style": "custom painted stars"}, "custom", "custom painted stars", GENERAL_NEGATIVE),
            ({"style_preset": "custom", "style": "custom painted stars", "negative": "explicit exclusion"}, "custom", "custom painted stars", "explicit exclusion"),
            ({"style": "unspecified custom medium"}, "custom", "unspecified custom medium", GENERAL_NEGATIVE),
        ]
        for saved, preset, style, negative in cases:
            with self.subTest(saved=saved):
                conn = db.get_conn()
                try:
                    with conn:
                        conn.execute("INSERT INTO render_configs VALUES('m3-book',?) ON CONFLICT(book_id) DO UPDATE SET config_json=excluded.config_json", (json.dumps({"provider": "mock", **saved}),))
                finally: conn.close()
                cfg = self.client.get(self.base + "/config").json()["config"]
                self.assertEqual((cfg["style_preset"], cfg["style"], cfg["negative"]), (preset, style, negative))


if __name__ == "__main__": unittest.main()
