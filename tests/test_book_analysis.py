"""Whole-book coverage, recovery and chronology using neutral, disposable fiction."""
import asyncio
import json
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fastapi.testclient import TestClient

from server.app import db
from server.app.ai import book_analysis as ba, settings, director
from server.app.ai.models import stable_json
from server.app.main import create_app
from server.app.manga.models import digest
from manga_fixture import seed


def output(text="Ari walked into the garden.", **changes):
    return {"synopsis": "Ari visits a garden and finds a key.", "summary": "Ari holds a key.", "entities": ["garden", "key"],
            "characters": [{"name": "Ari", "card": {"outfit": "invented value"}, "evidence": [
                {"field": "outfit", "value": "red coat", "quote": "red coat", "ch": 999}]}], **changes}


class BookAnalysisTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", root / "app.db"), ("server.app.config.DATA_DIR", root)]:
            p = patch(target, value); p.start(); self.addCleanup(p.stop)
        db.init_db(); seed()
        settings.save_settings({"base_url": "https://text.example/v1", "model": "gpt-oss-120b", "enabled": True, "api_key": "test-secret"})
        self.sql("UPDATE chapters SET paras_json=? WHERE book_id='m3-book'", (json.dumps(["Ari walked into the garden wearing a red coat."]),))
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)
        self.endpoint = "/api/books/m3-book/ai/analysis"

    def sql(self, sql, params=()):
        conn = db.get_conn()
        try:
            with conn:
                return [dict(r) for r in conn.execute(sql, params).fetchall()]
        finally:
            conn.close()

    def add_chapter(self, idx, text):
        self.sql("INSERT INTO chapters VALUES(?,?,?,?,?,?)", (f"ch{idx}", "m3-book", idx, f"Chapter {idx}", len(text), json.dumps([text])))

    def claim(self):
        conn = db.get_conn()
        try:
            with conn:
                job = ba.enqueue(conn, "m3-book", resume=True)
                conn.execute("UPDATE book_analysis SET status='running' WHERE book_id='m3-book'")
            return job
        finally:
            conn.close()

    def run_job(self, provider=None):
        job = self.claim()
        provider = provider or SimpleNamespace(complete=AsyncMock(return_value=output()))
        with patch.object(ba, "make_provider", return_value=provider):
            asyncio.run(ba.process(job))
        return self.client.get(self.endpoint).json()

    def test_chunk_coverage_including_long_paragraph_and_newlines(self):
        paras = ["a" * 20001, "", "b" * 13000, "尾声"]
        pieces = ba.chunks(paras)
        self.assertEqual("".join(p["text"] for p in pieces), "\n".join(paras))
        self.assertTrue(all(len(p["text"]) <= ba.CHUNK_CHARS for p in pieces))
        self.assertEqual(pieces[-1]["end_para"], 3)

    def test_all_chapters_cards_scenes_and_cache(self):
        self.add_chapter(1, "Ari walked home wearing a red coat.")
        provider = SimpleNamespace(complete=AsyncMock(return_value=output()))
        data = self.run_job(provider)
        self.assertEqual(data["analysis"]["status"], "done")
        self.assertEqual(data["analysis"]["completed"], 2)
        self.assertEqual(len(data["chapters"]), 2)
        card = json.loads(self.sql("SELECT card_json FROM characters WHERE name='Ari'")[0]["card_json"])
        self.assertEqual(card["outfit"], "red coat")
        self.assertEqual({e["ch"] for e in card["evidence"]}, {0, 1})
        self.assertNotIn("test-secret", stable_json(data))
        self.assertEqual(len(self.sql("SELECT * FROM scenes WHERE segment_id LIKE 'ba_%'")), 2)
        self.run_job(provider)
        self.assertEqual(provider.complete.await_count, 2)

    def test_partial_failure_resumes_without_repeating_success(self):
        self.add_chapter(1, "Ari waited by the gate.")
        first = SimpleNamespace(complete=AsyncMock(side_effect=[output(), RuntimeError("untrusted test-secret")]))
        data = self.run_job(first)
        self.assertEqual(data["analysis"]["status"], "error")
        self.assertEqual(data["analysis"]["completed"], 1)
        self.assertNotIn("test-secret", stable_json(data))
        second = SimpleNamespace(complete=AsyncMock(return_value=output()))
        self.assertEqual(self.run_job(second)["analysis"]["status"], "done")
        self.assertEqual(second.complete.await_count, 1)

    def test_pause_ignores_late_response_and_resume(self):
        async def pause(**kwargs):
            self.client.post(self.endpoint + "/pause")
            return output()
        job = self.claim()
        with patch.object(ba, "make_provider", return_value=SimpleNamespace(complete=pause)):
            with self.assertRaises(asyncio.CancelledError):
                asyncio.run(ba.process(job))
        self.assertEqual(self.client.get(self.endpoint).json()["analysis"]["status"], "paused")
        self.assertFalse(self.sql("SELECT * FROM chapter_summaries"))
        self.assertEqual(self.run_job()["analysis"]["status"], "done")

    def test_changed_source_invalidates_cache_and_late_write(self):
        self.run_job()
        self.sql("UPDATE chapters SET title='Changed' WHERE book_id='m3-book'")
        provider = SimpleNamespace(complete=AsyncMock(return_value=output()))
        self.run_job(provider)
        self.assertEqual(provider.complete.await_count, 1)
        async def change(**kwargs):
            self.sql("UPDATE chapters SET title='Changed again' WHERE book_id='m3-book'")
            return output()
        settings.save_settings({"model": "changed-model"})
        self.assertEqual(self.run_job(SimpleNamespace(complete=change))["analysis"]["status"], "error")
        self.assertEqual(json.loads(self.sql("SELECT summary_json FROM chapter_summaries")[0]["summary_json"])["title"], "Changed")

    def test_long_chapter_reduction_reads_every_piece_and_resumes_cached_pieces(self):
        self.sql("UPDATE chapters SET paras_json=?", (json.dumps(["Ari " + "x" * 14000]),))
        first = SimpleNamespace(complete=AsyncMock(side_effect=[output(), output(), output(), RuntimeError()]))
        self.assertEqual(self.run_job(first)["analysis"]["status"], "error")
        second = SimpleNamespace(complete=AsyncMock(return_value={"summary": "All three parts combined."}))
        data = self.run_job(second)
        self.assertEqual(data["analysis"]["status"], "done")
        self.assertEqual(second.complete.await_count, 1)
        self.assertEqual(len(json.loads(second.complete.await_args.kwargs["user"])["parts"]), 3)

    def test_frozen_identity_and_unsupported_evidence_are_preserved(self):
        original = self.sql("SELECT * FROM characters WHERE id='m3-char'")[0]
        self.sql("UPDATE chapters SET paras_json=?", (json.dumps(["Reference adult wears a red coat."]),))
        provider = SimpleNamespace(complete=AsyncMock(return_value=output(characters=[{"name": "Reference adult", "card": {"outfit": "red coat"}, "evidence": [{"field": "outfit", "value": "red coat", "quote": "red coat"}]}])))
        self.run_job(provider)
        updated = self.sql("SELECT * FROM characters WHERE id='m3-char'")[0]
        self.assertEqual(updated["identity_hash"], original["identity_hash"])
        self.assertEqual(json.loads(updated["card_json"])["outfit"], json.loads(original["card_json"])["outfit"])
        self.assertFalse(ba.normalize_part(output(), "Ari walks alone.", 0)["characters"][0]["card"])

    def test_context_relevance_excludes_future_and_stale_chapters(self):
        for idx in (1, 2): self.add_chapter(idx, "Ari returned with the key.")
        self.run_job()
        conn = db.get_conn()
        try:
            cid = conn.execute("SELECT id FROM characters WHERE name='Ari'").fetchone()[0]
            context = ba.relevant_context(conn, "m3-book", 1, cid, {"key_props": ["key"]})
            self.assertEqual([c["chapter"] for c in context["chapters"]], [0, 1])
            self.sql("UPDATE chapters SET title='Modified' WHERE idx=0")
            self.assertEqual([c["chapter"] for c in ba.relevant_context(conn, "m3-book", 1, cid, {})["chapters"]], [1])
        finally: conn.close()

    def test_auto_discovery_and_plan_readiness(self):
        conn = db.get_conn()
        try:
            with conn: ba.discover(conn)
        finally: conn.close()
        self.assertEqual(self.client.get(self.endpoint).json()["analysis"]["status"], "queued")
        response = self.client.post("/api/books/m3-book/manga/plan", json={"scene_id": "m3-scene", "form_id": "form_m3-char"})
        self.assertEqual(response.json()["error"]["code"], "book_analysis_pending")
        self.run_job()
        mock = SimpleNamespace(complete=AsyncMock(side_effect=[{"summary": "Garden scene."}, {"visual_prompt": "An adult stands in a garden."}]))
        with patch.object(director, "make_provider", return_value=mock):
            response = self.client.post("/api/books/m3-book/manga/plan", json={"scene_id": "m3-scene", "form_id": "form_m3-char"})
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["plan"]["scope"]["related_chapters"], [0])

    def test_disabled_auto_read_and_invalid_model_output(self):
        settings.save_settings({"auto_analyze": False})
        conn = db.get_conn()
        try:
            with conn: ba.discover(conn)
        finally: conn.close()
        self.assertFalse(self.sql("SELECT * FROM book_analysis"))
        data = self.run_job(SimpleNamespace(complete=AsyncMock(return_value={"not_a_summary": True})))
        self.assertEqual(data["analysis"]["status"], "error")
        self.assertFalse(data["chapters"])

    def test_scene_pagination_reaches_late_chapters(self):
        for idx in range(105):
            self.sql("INSERT INTO scenes(id,book_id,chapter_id,segment_id,plan_json,created_at,updated_at) VALUES(?,'m3-book',?,?,'{}',1,1)",
                     (f"late-scene-{idx}", idx, f"late-segment-{idx}"))
        first = self.client.get('/api/books/m3-book/ai/scenes?status=draft&limit=100').json()
        last = self.client.get('/api/books/m3-book/ai/scenes?status=draft&limit=100&offset=100').json()
        self.assertTrue(first['has_more']); self.assertFalse(last['has_more'])
        self.assertIn('late-scene-104', [s['id'] for s in last['scenes']])
        self.assertFalse({s['id'] for s in first['scenes']} & {s['id'] for s in last['scenes']})


if __name__ == "__main__": unittest.main()
