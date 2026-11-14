"""Creative context settings use isolated notes and mocked search transports."""
import asyncio
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient

from server.app import db
from server.app.ai import features, memory, web_search, settings, director
from server.app.ai.providers import OpenAICompatProvider
from server.app.main import create_app
from server.app.manga import service
from manga_fixture import seed


class CreativeFeatureTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in (("server.app.config.DATA_DIR", self.root), ("server.app.db.DB_PATH", self.root / "app.db")):
            mocked = patch(target, value); mocked.start(); self.addCleanup(mocked.stop)
        db.init_db(); seed()
        conn = db.get_conn()
        try:
            with conn:
                service.sync_base(conn, "m3-book")
        finally:
            conn.close()
        self.client = TestClient(create_app()); self.addCleanup(self.client.close)

    def note(self, **values):
        return memory.save({"title": "铜钥匙知识", "content": "铜钥匙是约定的信物", "tags": ["钥匙"], **values})

    def test_local_search_settings_show_saved_key_without_public_or_snapshot_leaks(self):
        original = self.client.get("/api/ai/features").json()["settings"]
        self.assertFalse(original["search"]["enabled"])
        saved = self.client.patch("/api/ai/features", json={"revision": original["revision"], "search": {
            "enabled": True, "base_url": "https://search.example", "api_key": "fixture-secret"}})
        self.assertEqual(saved.status_code, 200, saved.text)
        self.assertEqual(saved.json()["settings"]["search"]["api_key"], "fixture-secret")
        self.assertTrue(saved.json()["settings"]["search"]["key_configured"])
        reloaded = self.client.get("/api/ai/features")
        self.assertEqual(reloaded.json()["settings"]["search"]["api_key"], "fixture-secret")
        self.assertIn("no-store", reloaded.headers["cache-control"])
        self.assertNotIn("api_key", features.public()["search"])
        self.assertNotIn("fixture-secret", json.dumps(features.public()))
        self.assertNotIn("fixture-secret", (self.root / "ai-features.json").read_text(encoding="utf-8"))
        self.assertEqual(self.client.patch("/api/ai/features", json={"revision": 0, "prompts": {"text": "test"}}).status_code, 409)
        for payload in ({"search": {"max_results": 100}}, {"memory": {"max_items": True}},
                        {"memory": {"enabled": 1}}, {"prompts": {"text": "x" * 6001}},
                        {"search": {"base_url": "https://name:secret@search.example"}}, {"api_key": "bad"}):
            with self.subTest(payload=payload):
                self.assertEqual(self.client.patch("/api/ai/features", json=payload).status_code, 400)

    def test_paired_remote_reader_cannot_read_or_change_search_settings(self):
        features.save({"search": {"base_url": "https://search.example", "api_key": "local-search-secret"}})
        token = self.client.post("/api/security/token", json={}).json()["token"]
        with TestClient(self.client.app, base_url="http://192.168.1.10:8765", client=("192.168.1.20", 50001)) as remote:
            origin = {"Origin": "http://192.168.1.10:8765"}
            paired = remote.post("/api/auth/login", json={"token": token}, headers=origin)
            self.assertEqual(paired.status_code, 200, paired.text)
            for method in ("GET", "PATCH"):
                result = remote.request(method, "/api/ai/features", headers=origin,
                                        **({"json": {"search": {"api_key": "replacement"}}} if method == "PATCH" else {}))
                self.assertEqual(result.status_code, 403, result.text)
                self.assertNotIn("local-search-secret", result.text)
        self.assertEqual(self.client.get("/api/ai/features").json()["settings"]["search"]["api_key"], "local-search-secret")

    def test_enabling_keyed_search_requires_key_and_searxng_does_not(self):
        self.assertEqual(self.client.patch("/api/ai/features", json={"search": {"enabled": True}}).status_code, 400)
        self.assertEqual(self.client.patch("/api/ai/features", json={"search": {
            "enabled": True, "provider": "searxng", "base_url": "http://127.0.0.1:19999"}}).status_code, 200)

    def test_memory_crud_scope_revision_and_deletion(self):
        row = self.note(book_id="m3-book", character_id="m3-char", kind="personality")
        self.assertEqual(memory.list_all("m3-book", "钥匙")["memories"][0]["id"], row["id"])
        conflict = self.client.patch("/api/ai/memories/" + row["id"], json={"revision": 0, "content": "changed"})
        self.assertEqual(conflict.status_code, 409)
        changed = self.client.patch("/api/ai/memories/" + row["id"], json={"revision": 1, "enabled": False})
        self.assertEqual(changed.status_code, 200, changed.text)
        self.assertFalse(changed.json()["memory"]["enabled"])
        self.assertEqual(self.client.delete("/api/ai/memories/" + row["id"] + "?revision=1").status_code, 409)
        self.assertEqual(self.client.delete("/api/ai/memories/" + row["id"] + "?revision=2").status_code, 200)
        self.assertFalse(memory.list_all()["memories"])

    def test_memory_rejects_mismatched_character_and_unknown_fields(self):
        for values in ({"book_id": "missing"}, {"character_id": "m3-char"},
                       {"book_id": "m3-book", "character_id": "missing"}, {"kind": "unknown"}, {"api_key": "never"}):
            with self.subTest(values=values):
                result = self.client.post("/api/ai/memories", json={"title": "test", "content": "text", **values})
                self.assertEqual(result.status_code, 400, result.text)

    def test_retrieval_includes_relevant_scope_and_hides_unrelated_person(self):
        global_note = self.note()
        scoped = self.note(title="人物人格", book_id="m3-book", character_id="m3-char")
        hidden = self.note(title="disabled", enabled=False)
        conn = db.get_conn()
        try:
            selected = memory.retrieve(conn, "m3-book", ["m3-char"], "铜钥匙")
            self.assertEqual({m["id"] for m in selected}, {global_note["id"], scoped["id"]})
            without_character = memory.retrieve(conn, "m3-book", [], "铜钥匙")
            self.assertNotIn(scoped["id"], {m["id"] for m in without_character})
            self.assertNotIn(hidden["id"], {m["id"] for m in selected})
            features.save({"memory": {"enabled": False}})
            self.assertEqual(memory.retrieve(conn, "m3-book", ["m3-char"], "铜钥匙"), [])
        finally:
            conn.close()

    def test_retrieval_is_bounded_and_survives_new_connection(self):
        self.note(content="铜钥匙" * 1000, book_id="m3-book")
        self.note(title="第二条", content="铜钥匙" * 1000, book_id="m3-book")
        features.save({"memory": {"max_chars": 500, "max_items": 1}})
        conn = db.get_conn()
        try:
            selected = memory.retrieve(conn, "m3-book", [], "铜钥匙")
            self.assertEqual(len(selected), 1)
            self.assertLessEqual(sum(len(m["content"]) for m in selected), 500)
        finally:
            conn.close()
        self.assertEqual(len(memory.list_all()["memories"]), 2)

    def test_memory_changes_analysis_config_and_is_in_manual_material(self):
        settings.save_settings({"enabled": True, "base_url": "https://text.example/v1", "model": "fixture"})
        before = director.text_config()
        note = self.note(book_id="m3-book", character_id="m3-char")
        self.assertNotEqual(before, director.text_config())
        conn = db.get_conn()
        try:
            data = director.prepare_material(conn, "m3-book", {"scene_id": "m3-scene", "form_id": "form_m3-char"})
            self.assertEqual(data["long_term_memory"][0]["id"], note["id"])
        finally:
            conn.close()

    def test_text_preference_is_sent_without_replacing_schema_constraints(self):
        settings.save_settings({"base_url": "https://text.example/v1", "model": "fixture", "enabled": True})
        features.save({"prompts": {"text": "Prefer calm pacing."}})
        provider = OpenAICompatProvider("https://text.example/v1", "fixture", local_settings=True)
        with patch("server.app.ai.providers.request_json", return_value={"choices": [{"message": {"content": '{"ok":true}'}}]}) as request:
            provider._complete_sync("Original JSON structure must stay.", "text", {"type": "object"}, .2)
        system = request.call_args.kwargs["body"]["messages"][0]["content"]
        self.assertIn("Original JSON structure", system)
        self.assertIn("Prefer calm pacing.", system)
        self.assertIn("response_format", request.call_args.kwargs["body"])

    def test_image_preferences_are_snapshotted_before_queueing(self):
        features.save({"prompts": {"image": "fine ink lines", "negative": "watermark"}})
        conn = db.get_conn()
        try:
            with conn:
                job = service.enqueue(conn, "m3-book", {"kind": "reference", "form_id": "form_m3-char"})
            snap = json.loads(job["snapshot_json"])
            self.assertIn("fine ink lines", snap["prompt"])
            self.assertIn("Avoid: watermark", snap["prompt"])
            self.assertIn("watermark", snap["config"]["negative"])
        finally:
            conn.close()

    def mock_search(self, provider="searxng"):
        features.save({"search": {"enabled": True, "provider": provider, "base_url": "https://search.example", "api_key": "search-fixture-secret"}})
        values = {"results": [{"title": "Knowledge", "url": "https://source.example/item", "content": "Quoted fact"},
                              {"title": "Bad", "url": "javascript:alert(1)", "content": "bad"}]}
        if provider == "brave":
            values = {"web": values}
        opener = unittest.mock.MagicMock()
        opener.open.return_value.__enter__.return_value.read.return_value = json.dumps(values).encode()
        return patch("server.app.ai.web_search.urllib.request.build_opener", return_value=opener), opener

    def test_search_preserves_sources_rejects_script_links_and_hides_key(self):
        mocked, opener = self.mock_search()
        with mocked:
            result = self.client.post("/api/ai/web-search", json={"query": "铜钥匙知识"})
        self.assertEqual(result.status_code, 200, result.text)
        self.assertEqual(len(result.json()["results"]), 1)
        self.assertEqual(result.json()["results"][0]["url"], "https://source.example/item")
        self.assertNotIn("search-fixture-secret", result.text)
        self.assertNotIn("api_key", result.json())
        self.assertIn("format=json", opener.open.call_args.args[0].full_url)

    def test_tavily_and_brave_use_their_actual_protocols(self):
        for provider in ("tavily", "brave"):
            mocked, opener = self.mock_search(provider)
            with mocked:
                self.assertEqual(len(web_search.search("knowledge")["results"]), 1)
            request = opener.open.call_args.args[0]
            if provider == "tavily":
                self.assertEqual(request.get_header("Authorization"), "Bearer search-fixture-secret")
                self.assertEqual(json.loads(request.data)["query"], "knowledge")
            else:
                self.assertEqual(request.get_header("X-subscription-token"), "search-fixture-secret")
                self.assertIn("/res/v1/web/search?", request.full_url)

    def test_optional_enrichment_has_sources_cache_and_no_automatic_permanent_memory(self):
        mocked, opener = self.mock_search()
        features.save({"search": {"auto_enrich": True}})
        with mocked:
            first = asyncio.run(web_search.enrich({"text": "Original story"}, "knowledge"))
            second = asyncio.run(web_search.enrich({"text": "Original story"}, "knowledge"))
        self.assertEqual(first, second)
        self.assertEqual(opener.open.call_count, 1)
        self.assertEqual(first["text"], "Original story")
        self.assertIn("网页不是小说原文证据", first["web_context"]["usage"])
        self.assertFalse(memory.list_all()["memories"])

    def test_optional_search_failure_keeps_source_story(self):
        features.save({"search": {"enabled": True, "provider": "searxng", "auto_enrich": True}})
        with patch.object(web_search, "search", side_effect=service.MangaError("unavailable")):
            result = asyncio.run(web_search.enrich({"text": "Original"}, "knowledge"))
        self.assertEqual(result["text"], "Original")
        self.assertTrue(result["web_context"]["unavailable"])

    def test_disabled_search_performs_no_network_request(self):
        with patch("server.app.ai.web_search.urllib.request.build_opener") as opener:
            self.assertEqual(self.client.post("/api/ai/web-search", json={"query": "knowledge"}).status_code, 409)
            self.assertEqual(asyncio.run(web_search.enrich({"text": "Original"}, "knowledge")), {"text": "Original"})
            opener.assert_not_called()
