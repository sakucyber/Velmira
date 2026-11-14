"""Model library exercises real local persistence and an HTTP /models fixture only."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

from fastapi import FastAPI
from fastapi.responses import JSONResponse
from fastapi.testclient import TestClient

from server.app.ai import catalog, connections, routing, settings
from server.app.api.catalog import router
from server.app.manga.models import MangaError


class CatalogTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        data = patch("server.app.config.DATA_DIR", self.root)
        data.start(); self.addCleanup(data.stop)
        self.text = connections.save_connection({"id": "fixture-text", "label": "本地文本", "kind": "text",
            "models": ["legacy-model"], "base_url": "http://127.0.0.1:1/v1", "api_key": "catalog-private-key"})
        app = FastAPI(); app.include_router(router)
        @app.exception_handler(MangaError)
        async def error_handler(request, exc):
            return JSONResponse(status_code=exc.status, content={"error": {"code": exc.code, "message": str(exc)}})
        self.client = TestClient(app); self.addCleanup(self.client.close)

    def test_legacy_names_get_stable_ids_without_migration(self):
        first = self.client.get("/api/ai/catalog").json()
        model = first["models"][0]
        self.assertEqual(model["model"], "legacy-model")
        self.assertEqual(model["capabilities"], ["text"])
        self.assertIsNone(model["input_price"])
        self.assertEqual(model["id"], catalog.model_id("fixture-text", "legacy-model"))
        self.assertEqual(self.client.get("/api/ai/catalog").json(), first)
        self.assertFalse((self.root / "catalog.json").exists())
        self.assertNotIn("catalog-private-key", json.dumps(first))
        self.assertNotIn("key_dpapi", json.dumps(first))

    def test_create_edit_rename_and_delete_last_model(self):
        response = self.client.post("/api/ai/catalog/models", json={"connection_id": self.text["id"], "model": "vision-a",
            "label": "视觉模型", "capabilities": ["text", "vision"], "input_price": 1.2})
        self.assertEqual(response.status_code, 201, response.text)
        identity = response.json()["model"]["id"]
        changed = self.client.patch("/api/ai/catalog/models/" + identity, json={"model": "vision-b", "output_price": 3.4})
        self.assertEqual(changed.status_code, 200, changed.text)
        renamed = changed.json()["model"]
        self.assertNotEqual(renamed["id"], identity)
        self.assertEqual(renamed["input_price"], 1.2)
        self.assertEqual(renamed["output_price"], 3.4)
        self.assertEqual(renamed["label"], "视觉模型")
        self.assertEqual(self.client.get("/api/ai/catalog").json()["vendors"][0]["models"], ["legacy-model", "vision-b"])
        for item in list(catalog.public()["models"]):
            self.assertEqual(self.client.delete("/api/ai/catalog/models/" + item["id"]).status_code, 200)
        self.assertEqual(connections.public()[0]["models"], [])
        self.assertEqual(catalog.public()["models"], [])
        self.assertTrue(connections.public()[0]["key_configured"])

    def test_metadata_edit_does_not_change_private_key(self):
        original = settings.api_key(settings.load_settings("profile-" + self.text["id"]))
        identity = catalog.public()["models"][0]["id"]
        self.assertEqual(self.client.patch("/api/ai/catalog/models/" + identity, json={"label": "改名", "capabilities": ["text", "embedding"]}).status_code, 200)
        self.assertEqual(settings.api_key(settings.load_settings("profile-" + self.text["id"])), original)
        self.assertNotIn(original, (self.root / "catalog.json").read_text(encoding="utf-8"))

    def test_pool_references_prevent_model_rename_delete_and_vendor_delete(self):
        routing.save_config("text", {"enabled": False, "models": [{"connection_id": self.text["id"], "model": "legacy-model"}]})
        identity = catalog.public()["models"][0]["id"]
        self.assertIn("文本模型池（已关闭）", catalog.public()["models"][0]["assignments"])
        for response in [self.client.patch("/api/ai/catalog/models/" + identity, json={"model": "new-id"}),
                         self.client.delete("/api/ai/catalog/models/" + identity),
                         self.client.delete("/api/ai/catalog/vendors/" + self.text["id"])]:
            self.assertEqual(response.status_code, 409, response.text)
        # Display metadata remains editable without breaking configured routes.
        self.assertEqual(self.client.patch("/api/ai/catalog/models/" + identity, json={"label": "主模型"}).status_code, 200)
        self.assertEqual(connections.public()[0]["models"], ["legacy-model"])

    def test_duplicate_and_bad_metadata_do_not_mutate_connections(self):
        before = connections.entries()
        duplicate = self.client.post("/api/ai/catalog/models", json={"connection_id": self.text["id"], "model": "legacy-model"})
        self.assertEqual(duplicate.status_code, 409)
        for invalid in [{"capabilities": []}, {"capabilities": ["tools"]}, {"input_price": -1},
                        {"output_price": True}, {"model": "invalid\nmodel"}, {"api_key": "unexpected"}]:
            response = self.client.post("/api/ai/catalog/models", json={"connection_id": self.text["id"], "model": "new-model", **invalid})
            self.assertEqual(response.status_code, 400, response.text)
        self.assertEqual(connections.entries(), before)
        self.assertFalse((self.root / "catalog.json").exists())

    def test_vendor_deletion_removes_profile_and_models(self):
        identity = catalog.public()["models"][0]["id"]
        catalog.save_model({"label": "删除样例"}, identity)
        response = self.client.delete("/api/ai/catalog/vendors/" + self.text["id"])
        self.assertEqual(response.status_code, 200)
        self.assertEqual(catalog.public(), {"vendors": [], "models": []})
        self.assertFalse(settings._path("profile-" + self.text["id"]).exists())
        self.assertEqual(json.loads((self.root / "catalog.json").read_text(encoding="utf-8")), {})

    def test_can_create_an_empty_vendor_then_add_first_model(self):
        item = connections.save_connection({"label": "新厂商", "kind": "image", "models": [], "base_url": "https://image.example/v1"})
        self.assertEqual(item["models"], [])
        created = catalog.save_model({"connection_id": item["id"], "model": "image-new"})
        self.assertEqual(created["capabilities"], ["image"])
        self.assertEqual(connections.resolve(item["id"], "image-new", "image")["model"], "image-new")

    def test_missing_model_and_vendor_return_404(self):
        self.assertEqual(self.client.delete("/api/ai/catalog/models/not-found").status_code, 404)
        self.assertEqual(self.client.delete("/api/ai/catalog/vendors/not-found").status_code, 404)
        self.assertEqual(self.client.post("/api/ai/catalog/models", json={"connection_id": "not-found", "model": "a"}).status_code, 404)

    def test_probe_and_import_read_only_models_endpoint(self):
        seen = []
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.command, self.path, self.headers.get("Authorization")))
                body = json.dumps({"data": [{"id": "legacy-model"}, {"id": "new-remote"}, {"id": "new-remote"}, {"id": "bad\nmodel"}]}).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers(); self.wfile.write(body)
            def log_message(self, *args): pass
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True); worker.start()
        try:
            connections.save_connection({**{k: self.text[k] for k in ("id", "label", "kind", "models")},
                "base_url": "http://127.0.0.1:" + str(server.server_port) + "/v1", "api_key": "new-fixture-key"})
            before = catalog.public()
            remote = self.client.get("/api/ai/catalog/vendors/" + self.text["id"] + "/models")
            self.assertEqual(remote.json(), {"models": ["legacy-model", "new-remote"]})
            identity = before["models"][0]["id"]
            self.assertEqual(self.client.post("/api/ai/catalog/models/" + identity + "/probe").json()["ok"], True)
            self.assertEqual(catalog.public(), before)
            self.assertEqual(seen, [("GET", "/v1/models", "Bearer new-fixture-key")] * 2)
        finally:
            server.shutdown(); server.server_close(); worker.join(timeout=2)

    def test_remote_failure_never_echoes_gateway_secrets(self):
        with patch("server.app.ai.providers.request_json", side_effect=RuntimeError("gateway secret catalog-private-key")):
            response = self.client.get("/api/ai/catalog/vendors/" + self.text["id"] + "/models")
        self.assertEqual(response.status_code, 502)
        self.assertNotIn("gateway secret", response.text)
        self.assertNotIn("catalog-private-key", response.text)


if __name__ == "__main__":
    unittest.main()
