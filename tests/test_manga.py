"""M3 safety and recovery checks against a real disposable SQLite database."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient
from PIL import Image

from server.app import db, config
from server.app.main import create_app
from server.app.manga import queue
from server.app.manga.models import DEFAULTS, MangaError, build_workflow, resolve_form, validate_config
from manga_fixture import seed


class MangaTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.config.DATA_DIR", self.root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db(); seed(); queue._stop.clear()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.base = "/api/books/m3-book/manga"
        self.client.patch(self.base + "/config", json={"provider": "mock"})

    def api(self, path, data=None, method="POST", status=200):
        result = self.client.request(method, self.base + path, json=data)
        self.assertEqual(result.status_code, status, result.text)
        return result.json()

    def sql(self, sql, params=()):
        conn = db.get_conn()
        try:
            with conn:
                return [dict(r) for r in conn.execute(sql, params).fetchall()]
        finally:
            conn.close()

    def reference(self, accepted=True):
        raw = io.BytesIO(); Image.new("RGB", (64, 64), "white").save(raw, "PNG")
        result = self.client.post(self.base + "/forms/form_m3-char/refs", content=raw.getvalue())
        self.assertEqual(result.status_code, 201, result.text)
        ref = result.json()["ref"]
        if accepted: self.api("/refs/" + ref["id"], {"accepted": True}, "PATCH")
        return ref

    def enqueue(self, **payload):
        values = {"scene_id": "m3-scene", "character_id": "m3-char", **payload}
        return self.api("/jobs", values, status=201)["job"]

    def process(self, job):
        self.sql("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        queue.process(job["id"])
        return self.api("/jobs/" + job["id"], method="GET")["job"]

    def test_configuration_and_probe(self):
        for payload in [{"provider": "unknown"}, {"width": 257}, {"steps": True}, {"clip": "qwen2b"}, {"base_url": "http://user:pass@host"}, {"face_threshold": -1}, {"unknown": 1}, {"base_url": 5}]:
            self.api("/config", payload, "PATCH", 400)
        self.assertEqual(self.api("/probe")["provider"], "mock")
        self.assertEqual(self.client.get("/api/books/missing/manga/config").status_code, 404)

    def test_reference_is_a_required_manual_gate(self):
        self.api("/jobs", {"scene_id": "m3-scene", "character_id": "m3-char"}, status=409)
        self.reference(False)
        self.api("/jobs", {"scene_id": "m3-scene", "character_id": "m3-char"}, status=409)
        self.reference()
        job = self.enqueue()
        self.assertEqual(job["status"], "queued")
        self.sql("UPDATE characters SET frozen=0 WHERE id='m3-char'")
        self.api("/jobs", {"kind": "reference", "form_id": "form_m3-char"}, status=409)

    def test_unavailable_face_never_passes_and_manual_review_is_distinct(self):
        self.reference()
        job = self.process(self.enqueue())
        self.assertEqual(job["status"], "needs_review")
        self.assertIsNone(job["face_score"])
        self.assertEqual(job["history"][0]["result"]["reason"], "mock_output")
        reviewed = self.api("/jobs/" + job["id"] + "/approve")["job"]
        self.assertEqual(reviewed["status"], "approved")
        self.assertIsNone(reviewed["face_score"])
        self.assertEqual(self.client.get(job["url"]).status_code, 200)

    def test_reference_generation_does_not_auto_confirm(self):
        job = self.enqueue(kind="reference", form_id="form_m3-char")
        job = self.process(job)
        self.assertEqual(job["status"], "needs_review")
        refs = self.api("/forms", method="GET")["forms"][0]["refs"]
        self.assertEqual(len(refs), 1); self.assertEqual(refs[0]["accepted"], 0)
        self.api("/jobs/" + job["id"] + "/approve", status=409)

    def test_idempotent_submission_and_conflicting_reuse(self):
        self.reference()
        first = self.enqueue(request_id="same")
        self.assertEqual(first["id"], self.enqueue(request_id="same")["id"])
        self.api("/jobs", {"scene_id": "m3-scene", "character_id": "m3-char", "request_id": "same", "seed": 42}, status=409)

    def test_identity_upgrade_invalidates_images_and_refs(self):
        ref = self.reference(); job = self.process(self.enqueue())
        self.client.post("/api/books/m3-book/ai/characters/m3-char/unfreeze", json={"confirm": True})
        self.assertEqual(self.api("/jobs", method="GET")["jobs"][0]["status"], "stale")
        self.api("/refs/" + ref["id"], {"accepted": True}, "PATCH", 409)
        self.assertEqual(self.api("/forms", method="GET")["forms"][0]["version"], 2)

    def test_reference_revocation_and_scene_changes_invalidate_results(self):
        ref = self.reference(); job = self.process(self.enqueue())
        self.api("/refs/" + ref["id"], {"accepted": False}, "PATCH")
        self.api("/jobs/" + job["id"] + "/approve", status=409)
        self.assertEqual(self.api("/jobs/" + job["id"], method="GET")["job"]["status"], "stale")
        self.reference(); other = self.process(self.enqueue())
        self.sql("UPDATE scenes SET plan_json='{}' WHERE id='m3-scene'")
        self.api("/jobs/" + other["id"] + "/approve", status=409)
        self.assertEqual(self.api("/jobs", method="GET")["jobs"][0]["status"], "stale")

    def test_manual_character_edit_replaces_existing_identity_fields(self):
        self.reference()
        prefix = "/api/books/m3-book/ai/characters/m3-char"
        self.client.post(prefix + "/unfreeze", json={"confirm": True})
        response = self.client.patch(prefix, json={"card": {"hair_color": "brown"}})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["character"]["card"]["hair_color"], "brown")
        response = self.client.post(prefix + "/freeze", json={})
        self.assertIn("brown hair", response.json()["character"]["identity_block"])
        form = self.api("/forms", method="GET")["forms"][0]
        self.assertEqual(form["version"], 2)
        self.assertTrue(all(not r["accepted"] for r in form["refs"]))

    def test_invalid_and_cross_book_inputs(self):
        self.reference()
        self.sql("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES('other','Other','other.txt','other',1)")
        for data in [{"scene_id": []}, {"kind": []}, {"kind": "panel", "shot": []}, {"scene_id": "m3-scene", "character_id": "other"}, {"scene_id": "m3-scene", "character_id": "m3-char", "para": 10}]:
            response = self.client.post(self.base + "/jobs", json=data)
            self.assertIn(response.status_code, (400, 409), response.text)
        job = self.process(self.enqueue())
        self.assertEqual(self.client.get("/api/books/other/manga/assets/" + job["id"]).status_code, 404)
        self.assertEqual(self.client.get("/api/books/other/manga/jobs/" + job["id"]).status_code, 404)

    def test_multiple_people_require_splitting(self):
        self.reference()
        self.sql("UPDATE scenes SET plan_json=?", (json.dumps({"characters": [{}, {}], "character_ids": ["m3-char"]}),))
        self.api("/jobs", {"scene_id": "m3-scene", "character_id": "m3-char"}, status=409)

    def test_forms_switch_within_chapter_and_explicit_flashback(self):
        form = self.api("/forms", {"character_id": "m3-char", "form_key": "changed", "label": "Changed", "card": {"hair_color": "blue"}, "start_para": 4, "reversible": False}, status=201)["id"]
        self.api("/forms/" + form + "/actions/freeze")
        forms = self.api("/forms", method="GET")["forms"]
        self.assertEqual(resolve_form(forms, 0, 3)["form_key"], "base")
        self.assertEqual(resolve_form(forms, 0, 4)["id"], form)
        with self.assertRaises(MangaError): resolve_form(forms, 0, 4, "form_m3-char")
        self.assertEqual(resolve_form(forms, 0, 4, "form_m3-char", True)["form_key"], "base")
        self.api("/forms/" + form, {"label": "No"}, "PATCH", 409)
        self.api("/forms/" + form + "/actions/unfreeze", {"confirm": True})
        self.api("/forms/" + form, {"label": "Edited"}, "PATCH")
        self.assertEqual(next(f for f in self.api("/forms", method="GET")["forms"] if f["id"] == form)["version"], 2)

    def test_cancel_and_reroll_keep_history(self):
        self.reference(); job = self.enqueue()
        cancelled = self.api("/jobs/" + job["id"] + "/cancel")["job"]
        self.assertEqual(cancelled["status"], "cancelled")
        rerolled = self.api("/jobs/" + job["id"] + "/reroll")["job"]
        self.assertNotEqual(job["id"], rerolled["id"])
        self.assertNotEqual(job["snapshot"]["seed"], rerolled["snapshot"]["seed"])
        self.api("/jobs/" + rerolled["id"] + "/retry", status=409)

    def test_only_measured_score_can_auto_pass_and_low_scores_retry(self):
        self.reference()
        self.api("/config", {"provider": "comfyui", "max_attempts": 3}, "PATCH")
        from server.app.manga.provider import mock_image
        with patch("server.app.manga.queue.ComfyProvider") as provider, patch("server.app.manga.queue.evaluate_face") as face:
            instance = provider.return_value
            instance.probe.return_value = {"ok": True}
            instance.submit.return_value = ("fake-prompt", {})
            instance.result.return_value = mock_image(1, 256, 256)
            face.side_effect = [{"status": "measured", "score": .3}, {"status": "measured", "score": .8}]
            with patch.object(queue._stop, "wait", return_value=False): job = self.process(self.enqueue())
        self.assertEqual(job["status"], "done"); self.assertEqual(job["attempts"], 2)
        self.assertEqual([a["seed"] for a in job["history"]], [424242, 425251])
        self.assertEqual(job["face_score"], .8)

    def test_timeout_resumes_same_prompt_without_resubmitting(self):
        self.reference()
        self.api("/config", {"provider": "comfyui"}, "PATCH")
        job = self.enqueue()
        self.sql("UPDATE render_jobs SET status='error',attempts=1 WHERE id=?", (job["id"],))
        self.sql("INSERT INTO render_attempts(id,job_id,idx,seed,prompt_id) VALUES('attempt-resume',?,1,424242,'existing-prompt')", (job["id"],))
        self.api("/jobs/" + job["id"] + "/retry")
        from server.app.manga.provider import mock_image
        with patch("server.app.manga.queue.ComfyProvider") as provider, patch("server.app.manga.queue.evaluate_face", return_value={"status": "needs_review", "reason": "no_face"}):
            provider.return_value.result.return_value = mock_image(1, 256, 256)
            with patch.object(queue._stop, "wait", return_value=False): result = self.process(job)
            provider.return_value.submit.assert_not_called()
            provider.return_value.result.assert_called_with("existing-prompt")
        self.assertEqual(result["status"], "needs_review"); self.assertEqual(result["attempts"], 1)

    def test_workflow_uses_one_scaled_reference_and_no_vision_routing(self):
        graph = build_workflow(DEFAULTS, "frozen identity", 42, "ref.png")
        for node in ("4", "5"):
            self.assertEqual(graph[node]["inputs"]["edit_format"], "Moss-Image-Edit")
            self.assertEqual(graph[node]["inputs"]["images.image_0"], ["11", 0])
            self.assertNotIn("images.image_1", graph[node]["inputs"])
        self.assertEqual(graph["11"]["class_type"], "ImageScale")
        self.assertEqual(graph["7"]["inputs"]["seed"], 42)


if __name__ == "__main__":
    unittest.main()
