"""Automatic comics: causal ordering, durable progress and explicit draft provenance."""
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from fastapi.testclient import TestClient

from server.app import db
from server.app.ai import director
from server.app.main import create_app
from server.app.manga import automatic as a, queue, storyboard
from manga_fixture import seed


class AutomaticTests(unittest.TestCase):
    def setUp(self):
        p = patch("server.app.ai.retry.RETRY_DELAY", 0); p.start(); self.addCleanup(p.stop)
        p = patch("server.app.manga.queue.RETRY_DELAY", 0); p.start(); self.addCleanup(p.stop)
        tmp = tempfile.TemporaryDirectory(); self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.config.DATA_DIR", self.root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db(); seed(); queue._stop.clear()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.base = "/api/books/m3-book/manga"
        self.client.patch(self.base + "/config", json={"provider": "mock", "width": 256, "height": 256})
        self.client.patch("/api/ai/settings", json={"enabled": True, "auto_analyze": False, "model": "gpt-oss-120b", "base_url": "https://text.example/v1"})
        self.sql("UPDATE segments SET strategy='book-analysis'")
        self.sql("INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES('ch2','m3-book',1,'Later',20,'[\"Later sample\"]')")
        self.sql("INSERT INTO segments(id,book_id,chapter,idx,strategy,params_hash,start_para,end_para,text,features_json,score) SELECT 'seg2',book_id,1,0,strategy,params_hash,0,9,'Later sample','{}',1 FROM segments WHERE id='m3-seg'")
        self.sql("INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) SELECT 'scene2',book_id,1,'seg2',plan_json,1,1 FROM scenes WHERE id='m3-scene'")
        p = patch.object(a.analysis, "enqueue", return_value={"status": "done"}); p.start(); self.addCleanup(p.stop)
        self.story_calls = []
        async def story(**kw):
            material = json.loads(kw["user"]); self.story_calls.append(material)
            return {"panels": [{"kind": "solo", "subjects": [{"character_id": "m3-char", "position": "center", "pose": action}], "environment": "quiet park", "lighting": "daylight", "emotion": "", "source_quote": material["text"], "shot": shot, "action": action, "caption": caption, "dialogue": "", "speaker": ""}
                               for shot, action, caption in [("medium", "walk into the park", "走入公园。"), ("reaction", "stop beside the bench", "随后停下脚步。")]]}
        self.story_provider = SimpleNamespace(complete=AsyncMock(side_effect=story))
        p = patch.object(storyboard, "make_provider", return_value=self.story_provider); p.start(); self.addCleanup(p.stop)
        async def plan(material, config, **kwargs):
            return {"summary": {"summary": material["action"]}, "visual_prompt": "A single adult in a quiet park, coherent daylight, consistent clothing, face clearly visible."}
        self.plan = AsyncMock(side_effect=plan)
        p = patch.object(director, "write_plan", self.plan); p.start(); self.addCleanup(p.stop)

    def sql(self, sql, params=()):
        conn = db.get_conn()
        try:
            with conn: return [dict(r) for r in conn.execute(sql, params)]
        finally: conn.close()

    def call(self, enabled=True, chapter=0, expected=200):
        r = self.client.patch(self.base + "/auto", json={"enabled": enabled, "chapter": chapter})
        self.assertEqual(r.status_code, expected, r.text)
        return r.json().get("auto")

    def advance(self, count=1):
        for _ in range(count):
            job = self.sql("SELECT * FROM auto_manga")[0]
            if job["enabled"]: asyncio.run(a.advance(job))
            for render in self.sql("SELECT id FROM render_jobs WHERE status='queued'"):
                self.sql("UPDATE render_jobs SET status='running' WHERE id=?", (render["id"],))
                queue.process(render["id"])

    def finish(self):
        for _ in range(60):
            self.advance()
            value = self.client.get(self.base + "/auto").json()["auto"]
            if not value["enabled"]:
                self.assertEqual(value["status"], "done", value)
                return value
        self.fail("automatic manga did not finish")

    def test_whole_flow_order_carryover_drafts_and_reference_reuse(self):
        self.call()
        result = self.finish()
        self.assertEqual((result["pages"], result["completed_chapters"]), (2, 2))
        self.assertTrue(self.story_calls[1]["previous_panels"])
        self.assertEqual(self.story_calls[0]["text"], "Neutral sample")
        self.assertEqual(self.story_calls[1]["text"], "Later sample")
        jobs = self.sql("SELECT kind,status,face_score,snapshot_json FROM render_jobs")
        self.assertEqual(sum(j["kind"] == "reference" for j in jobs), 1)
        self.assertEqual(sum(j["kind"] == "panel" for j in jobs), 4)
        self.assertTrue(all(j["status"] == "needs_review" and j["face_score"] is None for j in jobs))
        self.assertEqual(self.story_provider.complete.await_count, 2)
        self.plan.assert_not_awaited()
        self.assertEqual(self.sql("SELECT accepted FROM form_refs"), [{"accepted": 0}])
        draft = self.client.get(self.base + "/auto/pages").json()
        self.assertEqual([p["chapter"] for p in draft["pages"]], [0, 1])
        self.assertTrue(all(p["status"] == "draft" and p["pending_review"] == 2 for p in draft["pages"]))
        self.assertEqual(self.client.get(draft["pages"][0]["url"]).status_code, 200)
        pid = self.sql("SELECT id FROM render_jobs WHERE kind='panel'")[0]["id"]
        self.assertEqual(self.client.post(self.base + "/pages", json={"panels": [{"job_id": pid}]}).status_code, 409)
        self.assertEqual(self.client.post(self.base + "/exports").status_code, 400)

    def test_current_chapter_resume_restart_and_no_duplicate_jobs(self):
        first = self.call(chapter=1)
        self.advance(4)
        jobs = len(self.sql("SELECT * FROM render_jobs"))
        self.call()
        self.call(False)
        self.advance(4)
        self.assertEqual(len(self.sql("SELECT * FROM render_jobs")), jobs)
        resumed = self.call(chapter=0)
        self.assertEqual(resumed["run_id"], first["run_id"])
        self.assertEqual(resumed["start_chapter"], 1)
        db.init_db()  # durable on-disk state survives a restart
        result = self.finish()
        self.assertEqual((result["pages"], result["completed_chapters"]), (1, 1))
        self.assertEqual(len(self.sql("SELECT * FROM render_jobs")), 3)
        self.assertEqual(self.story_provider.complete.await_count, 1)

    def test_preview_gate_edit_and_confirm_resume_without_more_text_calls(self):
        self.client.patch(self.base + '/config', json={'review_storyboards': True})
        self.call(chapter=1); self.advance(3)
        state = self.client.get(self.base+'/auto').json()['auto']
        self.assertEqual(state['status'], 'reviewing')
        self.assertEqual(self.sql('SELECT * FROM render_jobs'), [])
        url = self.base+'/storyboards/'+state['board_id']
        board = self.client.get(url).json()['storyboard']; board['panels'][0]['caption']='走入公园。'
        changed = self.client.patch(url,json={'revision':board['revision'],'panels':board['panels']}).json()['storyboard']
        self.assertEqual(self.client.post(url+'/approve',json={'revision':board['revision']}).status_code,409)
        self.assertEqual(self.client.post(url+'/approve',json={'revision':changed['revision']}).status_code,200)
        self.finish(); self.assertEqual(self.story_provider.complete.await_count,1); self.plan.assert_not_awaited()

    def test_legacy_unfinished_page_resumes_original_plan_without_replanning(self):
        self.call(chapter=1);self.advance(3)
        state=json.loads(self.sql('SELECT state_json FROM auto_manga')[0]['state_json'])
        for item in state['story']:
            item['character_id']=item['subjects'][0]['character_id']
            if item.get('form_ids'):item['form_id']=item['form_ids'][0]
        state.pop('board_id');state.pop('board_revision')
        self.sql('UPDATE auto_manga SET state_json=?',(json.dumps(state),))
        calls=self.story_provider.complete.await_count
        self.finish()
        self.assertEqual(self.story_provider.complete.await_count,calls)
        self.assertGreater(self.plan.await_count,0)

    def test_streaming_generates_at_ten_waits_for_late_chapter_and_keeps_pause_owner(self):
        for idx in range(2, 12):
            self.sql("INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES(?,'m3-book',?,'Later',12,'[\"Later sample\"]')", (f"ch{idx + 1}", idx))
        self.sql("INSERT INTO book_analysis(book_id,run_id,status,source_hash,config_json,total,completed,updated_at) VALUES('m3-book','owned','paused','test','{}',12,9,1)")
        progress = {"status": "running", "run_id": "owned", "completed": 9, "total": 12}
        with patch.object(a.analysis, "enqueue", return_value=progress):
            self.call()
            self.sql("UPDATE book_analysis SET status='running'")
            self.advance(2)
            self.story_provider.complete.assert_not_awaited()
            self.assertEqual(self.sql("SELECT status FROM auto_manga")[0]["status"], "analyzing")
            progress["completed"] = 10
            self.sql("UPDATE book_analysis SET completed=10")
            for _ in range(20):
                self.advance()
                if self.sql("SELECT id FROM auto_manga_pages"): break
            self.assertEqual(len(self.sql("SELECT id FROM auto_manga_pages")), 1)
            value = self.client.get(self.base + "/auto").json()["auto"]
            self.assertEqual(value["analysis"], {"status": "running", "completed": 10, "total": 12})
            self.advance()  # Advance to the next chapter without losing analysis ownership.
            state = json.loads(self.sql("SELECT state_json FROM auto_manga")[0]["state_json"])
            self.assertEqual(state["analysis_run"], "owned")
            calls = self.story_provider.complete.await_count
            self.sql("UPDATE auto_manga SET chapter=10")
            self.advance(2)
            self.assertEqual(self.story_provider.complete.await_count, calls)
            self.assertEqual(self.sql("SELECT status FROM auto_manga")[0]["status"], "analyzing")
            self.call(False)
            self.assertEqual(self.sql("SELECT status FROM book_analysis")[0]["status"], "paused")

    def test_logs_persist_are_bounded_and_pages_follow_chapter(self):
        from server.app import task_log
        self.call(); self.finish()
        result = self.client.get(self.base + "/auto/logs").json()
        self.assertTrue(result["logs"])
        self.assertTrue(any("漫画页已保存" in item["message"] for item in result["logs"]))
        cursor = result["cursor"]
        self.assertEqual(self.client.get(self.base + "/auto/logs?after=" + str(cursor)).json()["logs"], [])
        pages = self.client.get(self.base + "/auto/pages?chapter=1&limit=1").json()
        self.assertEqual(pages["total"], 1)
        self.assertEqual([p["chapter"] for p in pages["pages"]], [1])
        self.assertEqual(self.client.get(self.base + "/auto/pages?chapter=2").json()["total"], 0)
        self.assertEqual(self.client.get("/api/books/missing/manga/auto/logs").status_code, 404)
        conn = db.get_conn()
        try:
            with conn:
                for number in range(1005): task_log.record(conn, "m3-book", result["run_id"], "test", str(number))
        finally: conn.close()
        self.assertEqual(self.sql("SELECT COUNT(*) AS n FROM task_logs")[0]["n"], 1000)
        recent = self.client.get(self.base + "/auto/logs?limit=2").json()
        self.assertEqual([x["message"] for x in recent["logs"]], ["1003", "1004"])

    def test_story_recovers_on_third_attempt(self):
        self.call()
        story = {"panels": [{"kind": "solo", "subjects": [{"character_id": "m3-char", "position": "center", "pose": "walk"}], "environment": "park", "lighting": "", "emotion": "", "source_quote": "Neutral sample", "shot": "medium", "action": "walk", "caption": "走入公园。", "dialogue": "", "speaker": ""}]}
        self.story_provider.complete.side_effect = [RuntimeError("private"), {"panels": []}, story]
        self.advance()
        self.assertEqual(self.story_provider.complete.await_count, 3)
        self.assertEqual(self.sql("SELECT status FROM auto_manga")[0]["status"], "generating")
        self.assertNotIn("private", self.client.get(self.base + "/auto/logs").text)

    def test_pause_during_model_request_discards_late_work(self):
        self.call()
        async def delayed(**kw):
            self.call(False)
            return {"panels": [{"character_id": "m3-char", "shot": "medium", "action": "walk", "caption": "行走。"}]}
        self.story_provider.complete.side_effect = delayed
        self.advance()
        self.assertEqual(self.sql("SELECT status FROM auto_manga")[0]["status"], "paused")
        self.assertEqual(self.sql("SELECT * FROM render_jobs"), [])

    def test_error_does_not_retry_forever_or_leak_provider_secrets(self):
        self.call()
        self.story_provider.complete.side_effect = RuntimeError("secret-credential")
        self.advance(10)
        result = self.client.get(self.base + "/auto").json()["auto"]
        self.assertEqual(result["status"], "error")
        self.assertFalse(result["enabled"])
        self.assertNotIn("secret-credential", json.dumps(result))
        self.assertEqual(self.story_provider.complete.await_count, 3)

    def test_source_change_and_reference_revocation_stop_work(self):
        self.call(); self.advance(4)
        ref = self.sql("SELECT ref_id FROM auto_manga_refs")[0]["ref_id"]
        self.client.patch(self.base + "/refs/" + ref, json={"accepted": False})
        self.advance(3)
        self.assertEqual(self.sql("SELECT status FROM auto_manga")[0]["status"], "error")
        self.assertEqual(self.sql("SELECT * FROM auto_manga_refs"), [])
        self.sql("UPDATE chapters SET paras_json='[\"Changed\"]' WHERE idx=0")
        self.call(expected=409)

    def test_strict_inputs_and_cross_book_assets(self):
        for body in [{"enabled": 1}, {"enabled": True, "chapter": True}, {"enabled": True, "chapter": 50}, {"enabled": True, "extra": "x"}]:
            self.assertEqual(self.client.patch(self.base + "/auto", json=body).status_code, 400)
        self.assertEqual(self.client.get("/api/books/missing/manga/auto").status_code, 404)
        scene = {"text": "Hello"}
        cast = [{"id": "a", "name": "A"}]
        with self.assertRaises(ValueError):
            a.normalize_story({"panels": [{"character_id": "b", "shot": "medium", "caption": "x"}]}, scene, cast)
        with self.assertRaises(ValueError):
            a.normalize_story({"panels": [{"character_id": "a", "shot": "medium", "caption": "x", "dialogue": "invented", "speaker": "A"}]}, scene, cast)

    def test_late_epoch_cannot_reenable_paused_work(self):
        self.call(); old = self.sql("SELECT * FROM auto_manga")[0]
        self.call(False); self.call()
        conn = db.get_conn()
        try:
            with self.assertRaises(a.Paused), conn:
                a.save(conn, old, {}, "generating", "late")
        finally: conn.close()
        self.assertNotEqual(self.sql("SELECT message FROM auto_manga")[0]["message"], "late")

    def test_history_and_other_book_cannot_read_draft(self):
        old = self.call(chapter=1); self.finish()
        self.call(chapter=0)
        self.assertEqual(self.client.get(self.base + "/auto/pages").json()["pages"], [])
        history = self.client.get(self.base + "/auto/runs").json()["runs"]
        self.assertEqual(history[0]["run_id"], old["run_id"])
        page = self.client.get(self.base + "/auto/pages?run_id=" + old["run_id"]).json()["pages"][0]
        self.sql("INSERT INTO books(id,title,file_path,sha256,created_at) VALUES('other','Other','other.txt','other',1)")
        self.assertEqual(self.client.get(page["url"].replace('m3-book', 'other')).status_code, 404)

    def test_pause_owned_analysis_but_preserve_independent_analysis(self):
        # Store a real pending analysis record while controlling its enqueue result.
        self.sql("INSERT INTO book_analysis(book_id,run_id,status,source_hash,config_json,total,updated_at) VALUES('m3-book','owned','paused','test','{}',2,1)")
        with patch.object(a.analysis, "enqueue", return_value={"status": "queued", "run_id": "owned"}):
            self.call()
        self.sql("UPDATE book_analysis SET status='running'")
        self.call(False)
        self.assertEqual(self.sql("SELECT status FROM book_analysis")[0]["status"], "paused")
        self.sql("UPDATE book_analysis SET status='running',run_id='independent'")
        self.call(); self.call(False)
        self.assertEqual(self.sql("SELECT status FROM book_analysis")[0]["status"], "running")

    def test_unfrozen_identity_uses_only_evidence_at_chapter(self):
        self.sql("UPDATE characters SET frozen=0,identity_block=NULL,identity_hash=NULL,card_json=?", (json.dumps({"hair_color": "silver", "evidence": [
            {"field": "hair_color", "value": "black", "ch": 0}, {"field": "hair_color", "value": "silver", "ch": 4}]}),))
        self.call(chapter=1); self.finish()
        forms = self.sql("SELECT card_json FROM character_forms WHERE form_key LIKE 'auto_%'")
        self.assertEqual(json.loads(forms[0]["card_json"])["hair_color"], "black")

    def test_changed_outfit_reference_preserves_previous_character_face(self):
        self.sql("UPDATE characters SET frozen=0,identity_block=NULL,identity_hash=NULL,card_json=?", (json.dumps({"evidence": [
            {"field": "outfit", "value": "red jacket", "ch": 0}, {"field": "outfit", "value": "blue jacket", "ch": 1, "quote": "Later sample"}]}),))
        self.call(); self.finish()
        refs = self.sql("SELECT snapshot_json FROM render_jobs WHERE kind='reference' ORDER BY created_at,rowid")
        self.assertEqual(len(refs), 2)
        first, second = [json.loads(r["snapshot_json"]) for r in refs]
        self.assertIsNone(first["reference"])
        self.assertIsNotNone(second["reference"])
        self.assertEqual(second["reference"]["form_id"], first["form"]["id"])
        self.assertIn("blue jacket", second["prompt"])
        self.assertIn("same character and face as reference", second["prompt"])

    def test_no_cast_generates_real_environment_with_review_required(self):
        self.sql("UPDATE scenes SET plan_json='{}' WHERE chapter_id=1")
        self.story_provider.complete.side_effect = None
        self.story_provider.complete.return_value = {"panels": [{"kind": "environment", "subjects": [], "shot": "wide", "caption": "夜幕降临，街道逐渐安静下来。", "action": "An empty street becomes quiet", "environment": "street", "lighting": "night", "emotion": "", "source_quote": "Later sample", "dialogue": "", "speaker": ""}]}
        self.call(chapter=1)
        self.assertEqual(self.finish()["pages"], 1)
        jobs = self.sql("SELECT form_id,snapshot_json FROM render_jobs")
        self.assertEqual(len(jobs), 1)
        self.assertIsNone(jobs[0]["form_id"])
        self.assertIn("no people", json.loads(jobs[0]["snapshot_json"])["prompt"])
        self.assertEqual(self.client.get(self.base + "/auto/pages").json()["pages"][0]["pending_review"], 1)

    def test_diagnose_maps_provider_status_without_network_or_raw_logs(self):
        self.call()
        self.call(False)
        for code, title in [("provider_auth", "API 密钥无效或已过期"),
                            ("provider_rate_limit", "API 请求过于频繁"),
                            ("provider_billing", "API 账户余额或额度不足"),
                            ("provider_server_error", "AI 服务暂时故障")]:
            state = json.dumps({"last_error_code": code, "last_error_message": "Bearer sk-secret"})
            self.sql("UPDATE auto_manga SET status='error',enabled=0,message='Bearer sk-secret',state_json=?", (state,))
            response = self.client.get(self.base + "/auto/diagnose")
            self.assertEqual(response.status_code, 200, response.text)
            body = response.json()
            self.assertEqual(body["checks"][0]["code"], code)
            self.assertEqual(body["checks"][0]["title"], title)
            self.assertNotIn("sk-secret", json.dumps(body))
            self.assertNotIn("Bearer", json.dumps(body))

    def test_provider_http_failure_survives_automatic_task_and_diagnosis(self):
        from server.app.ai.providers import ProviderRequestError
        for status, code, detail in [(401, "provider_auth", "API Key"), (429, "provider_rate_limit", "并发")]:
            self.call()
            self.story_provider.complete.side_effect = ProviderRequestError(
                "upstream Bearer sk-secret", status_code=status, error_code="http_error")
            self.advance()
            self.sql("INSERT INTO task_logs(book_id,run_id,stage,level,message,created_at) VALUES('m3-book','unknown','rendering','error','Bearer sk-secret raw request',1)")
            with patch("urllib.request.build_opener") as network:
                response = self.client.get(self.base + "/auto/diagnose")
                network.assert_not_called()
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.json()["checks"][0]["code"], code)
            self.assertIn(detail, response.json()["checks"][0]["detail"])
            self.assertNotIn("sk-secret", response.text)
            self.assertNotIn("raw request", response.text)


if __name__ == "__main__":
    unittest.main()
