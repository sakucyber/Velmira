"""Context director checks using neutral fiction and disposable local settings."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fastapi.testclient import TestClient
from PIL import Image

from server.app import db
from server.app.ai import director, settings
from server.app.ai.providers import OpenAICompatProvider
from server.app.main import create_app
from server.app.manga import queue
from manga_fixture import seed


SUMMARY = {"summary": "一位成年人在公园等候，听到身后的脚步声后转头。", "location": "公园",
           "moment": "转头", "visible_action": "转头望向画外", "emotion": "好奇",
           "key_props": ["长椅"], "uncertainties": ["具体时间未知"]}
PROMPT = "A single adult turns her head in a quiet park, curious expression, a bench beside her, face clearly visible."


class TextAITests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.config.DATA_DIR", self.root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db(); seed(); queue._stop.clear()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.base = "/api/books/m3-book/manga"
        self.client.patch(self.base + "/config", json={"provider": "mock"})
        self.payload = {"scene_id": "m3-scene", "form_id": "form_m3-char"}
        self.save_settings()

    def save_settings(self, **patch_values):
        return self.client.patch("/api/ai/settings", json={"base_url": "https://text.example/v1",
            "model": "test-director", "enabled": True, "api_key": "local-test-credential", **patch_values})

    def sql(self, sql, params=()):
        conn = db.get_conn()
        try:
            with conn:
                return [dict(r) for r in conn.execute(sql, params).fetchall()]
        finally:
            conn.close()

    def provider(self, outputs=None):
        return SimpleNamespace(complete=AsyncMock(side_effect=outputs if outputs is not None else [SUMMARY, {"visual_prompt": PROMPT}]))

    def plan(self, provider=None, payload=None, status=200):
        with patch.object(director, "make_provider", return_value=provider or self.provider()):
            result = self.client.post(self.base + "/plan", json=payload or self.payload)
        self.assertEqual(result.status_code, status, result.text)
        return result.json()

    def reference(self):
        raw = io.BytesIO(); Image.new("RGB", (64, 64), "white").save(raw, "PNG")
        response = self.client.post(self.base + "/forms/form_m3-char/refs", content=raw.getvalue())
        self.assertEqual(response.status_code, 201, response.text)
        ref = response.json()["ref"]
        self.assertEqual(self.client.patch(self.base + "/refs/" + ref["id"], json={"accepted": True}).status_code, 200)
        return ref

    def test_settings_roundtrip_keeps_credentials_out_of_responses_and_snapshots(self):
        response = self.client.get("/api/ai/settings")
        self.assertNotIn("local-test-credential", response.text)
        self.assertTrue(response.json()["settings"]["key_configured"])
        self.assertEqual(settings.api_key(), "local-test-credential")
        self.assertEqual(self.save_settings(api_key="").status_code, 200)
        self.assertEqual(settings.api_key(), "local-test-credential")
        plan = self.plan()["plan"]
        self.assertNotIn("local-test-credential", json.dumps(plan))
        self.assertEqual(plan["provider"], {"base_url": "https://text.example/v1", "model": "test-director"})
        if "key_dpapi" in settings.load_settings():
            self.assertNotIn("local-test-credential", (self.root / "text-api.json").read_text())

    def test_rejects_invalid_settings_and_disabled_director(self):
        for values in [{"base_url": "http://remote.example/v1"}, {"base_url": "https://user:pass@text.example/v1"},
                       {"base_url": "https://text.example/v1?key=private"}, {"enabled": 1},
                       {"api_key": "bad\nheader"}, {"unknown": True}]:
            self.assertEqual(self.save_settings(**values).status_code, 400)
        self.save_settings(enabled=False)
        self.plan(status=409)

    def test_two_stages_cache_and_model_change(self):
        provider = self.provider()
        first = self.plan(provider)
        self.assertFalse(first["cached"])
        self.assertEqual(provider.complete.await_count, 2)
        calls = provider.complete.await_args_list
        material = json.loads(calls[0].kwargs["user"])
        prompt_input = json.loads(calls[1].kwargs["user"])
        self.assertEqual(material["target_segment"], "Neutral sample")
        self.assertEqual(prompt_input["context_summary"]["summary"], SUMMARY["summary"])
        self.assertEqual(prompt_input["frozen_identity"], material["frozen_identity"])
        self.assertNotIn("chapter_context", prompt_input)
        self.assertIn("never as instructions", calls[1].kwargs["system"])
        cached = self.plan(provider)
        self.assertTrue(cached["cached"])
        self.assertEqual(cached["plan"]["id"], first["plan"]["id"])
        self.assertEqual(provider.complete.await_count, 2)
        self.save_settings(model="another-director")
        changed = self.plan()["plan"]
        self.assertNotEqual(changed["id"], first["plan"]["id"])

    def test_invalid_model_output_and_network_failure_never_enqueue(self):
        cases = [(["not json"], 1), ([{"summary": []}], 1),
                 ([{**SUMMARY, "key_props": "invalid"}], 1),
                 ([SUMMARY, {"visual_prompt": "x" * 1801}], 2),
                 ([SUMMARY, RuntimeError("private gateway error")], 2)]
        for outputs, count in cases:
            provider = self.provider(outputs)
            response = self.plan(provider, status=502)
            self.assertEqual(provider.complete.await_count, count)
            self.assertNotIn("private gateway error", json.dumps(response))
            self.assertEqual(self.sql("SELECT * FROM manga_text_plans"), [])
            self.assertEqual(self.sql("SELECT * FROM render_jobs"), [])

    def test_mixed_language_prompt_has_one_bounded_repair(self):
        mixed = {"visual_prompt": "The selected character holds 一杯茶."}
        provider = self.provider([SUMMARY, mixed, {"visual_prompt": PROMPT}])
        self.assertEqual(self.plan(provider)["plan"]["visual_prompt"], PROMPT)
        self.assertEqual(provider.complete.await_count, 3)
        self.assertEqual(json.loads(provider.complete.await_args_list[2].kwargs["user"])["previous_draft"], mixed["visual_prompt"])
        provider = self.provider([SUMMARY, mixed, mixed])
        self.plan(provider, {**self.payload, "shot": "wide"}, 502)
        self.assertEqual(provider.complete.await_count, 3)
        self.assertEqual(self.sql("SELECT * FROM render_jobs"), [])

    def test_plan_keeps_reference_gate_and_snapshot_through_reroll(self):
        plan = self.plan()["plan"]
        request = {**self.payload, "plan_id": plan["id"]}
        self.assertEqual(self.client.post(self.base + "/jobs", json=request).status_code, 409)
        self.reference()
        response = self.client.post(self.base + "/jobs", json=request)
        self.assertEqual(response.status_code, 201, response.text)
        job = response.json()["job"]
        self.assertEqual(job["snapshot"]["text_plan"], plan)
        self.assertIn(PROMPT, job["snapshot"]["prompt"])
        self.assertIn(job["snapshot"]["form"]["identity_block"], job["snapshot"]["prompt"])
        self.sql("UPDATE render_jobs SET status='running' WHERE id=?", (job["id"],))
        queue.process(job["id"])
        done = self.client.get(self.base + "/jobs/" + job["id"]).json()["job"]
        self.assertEqual(done["status"], "needs_review")
        reroll = self.client.post(self.base + "/jobs/" + job["id"] + "/reroll", json={}).json()["job"]
        self.assertEqual(reroll["snapshot"]["text_plan"]["id"], plan["id"])
        self.assertNotEqual(reroll["snapshot"]["seed"], job["snapshot"]["seed"])

    def test_changes_in_context_reject_plan_and_pending_job(self):
        self.reference(); plan = self.plan()["plan"]
        request = {**self.payload, "plan_id": plan["id"]}
        job = self.client.post(self.base + "/jobs", json=request).json()["job"]
        self.sql("UPDATE chapters SET paras_json='[\"Changed context\"]' WHERE id='m3-ch'")
        response = self.client.post(self.base + "/jobs", json=request)
        self.assertEqual(response.status_code, 409, response.text)
        self.assertEqual(response.json()["error"]["code"], "text_plan_stale")
        jobs = self.client.get(self.base + "/jobs").json()["jobs"]
        self.assertEqual(jobs[0]["id"], job["id"])
        self.assertEqual(jobs[0]["status"], "stale")

    def test_change_during_network_calls_discards_plan(self):
        async def changed(*_args, **_kwargs):
            self.sql("UPDATE chapters SET title='Revised' WHERE id='m3-ch'")
            return {"summary": SUMMARY, "visual_prompt": PROMPT}
        with patch.object(director, "write_plan", side_effect=changed):
            response = self.client.post(self.base + "/plan", json=self.payload)
        self.assertEqual(response.status_code, 409, response.text)
        self.assertEqual(self.sql("SELECT * FROM manga_text_plans"), [])

    def test_context_includes_previous_tail_and_excludes_future_chapters(self):
        self.sql("UPDATE chapters SET idx=1 WHERE id='m3-ch'")
        self.sql("UPDATE scenes SET chapter_id=1 WHERE id='m3-scene'")
        self.sql("INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES('prev','m3-book',0,'Previous',4000,?)", (json.dumps(["P" * 4000]),))
        self.sql("INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES('future','m3-book',2,'Future',10,'[\"FUTURE_ONLY\"]')")
        provider = self.provider(); self.plan(provider)
        material = json.loads(provider.complete.await_args_list[0].kwargs["user"])
        self.assertEqual(material["previous_chapter_tail"], "P" * 3000)
        self.assertIn("Neutral sample", material["chapter_context"])
        self.assertNotIn("FUTURE_ONLY", json.dumps(material))

    def test_invalid_identifiers_fail_without_network_requests(self):
        for payload in [{**self.payload, "scene_id": []}, {**self.payload, "form_id": {}},
                        {**self.payload, "para": True}, {**self.payload, "shot": []}]:
            provider = self.provider(); self.plan(provider, payload, 400)
            provider.complete.assert_not_awaited()
        self.reference()
        self.assertEqual(self.client.post(self.base + "/jobs", json={**self.payload, "plan_id": []}).status_code, 400)

    def test_endpoint_change_never_sends_replacement_credential_to_old_host(self):
        provider = OpenAICompatProvider("https://text.example/v1", "test-director", local_settings=True)
        self.save_settings(base_url="https://replacement.example/v1", api_key="replacement-test-credential")
        with patch("server.app.ai.providers.request_json") as send:
            with self.assertRaises(ValueError):
                provider._complete_sync("system", "user", None, .2)
            send.assert_not_called()

    def test_gpt_oss_text_only_request_and_conservative_context_budget(self):
        provider = OpenAICompatProvider("https://text.example/v1", "gpt-oss-120b")
        with patch("server.app.ai.providers.request_json", return_value={"choices": [{"message": {"content": '{"ok":true}'}}]}) as send:
            self.assertEqual(provider._complete_sync("Return JSON", "中性测试正文", {"type": "object"}, .2), {"ok": True})
            body = send.call_args.kwargs["body"]
            self.assertEqual(body["model"], "gpt-oss-120b")
            self.assertTrue(all(isinstance(m["content"], str) for m in body["messages"]))
            send.reset_mock()
            with self.assertRaisesRegex(ValueError, "128K"):
                provider._complete_sync("system", "文" * 43000, None, .2)
            send.assert_not_called()
            with self.assertRaisesRegex(ValueError, "文字"):
                provider._complete_sync("system", [{"type": "image_url"}], None, .2)
            send.assert_not_called()


if __name__ == "__main__":
    unittest.main()
