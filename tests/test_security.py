"""Real ASGI security boundary; no network service or model API calls."""
from pathlib import Path
import io
import tempfile
import unittest
import zipfile
from urllib.parse import unquote
from unittest.mock import patch

from fastapi import FastAPI
from fastapi.responses import Response
from fastapi.staticfiles import StaticFiles
from fastapi.testclient import TestClient
from PIL import Image

from server.app import config, security
from server.app.api.security import router


class SecurityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for target, value in (("server.app.config.DATA_DIR", self.root), ("server.app.security._login_attempts", {})):
            item = patch(target, value); item.start(); self.addCleanup(item.stop)
        self.app = FastAPI()
        security.install_security(self.app)
        self.app.include_router(router)
        @self.app.get("/api/health")
        def health(): return {"ok": True}
        @self.app.get("/api/books")
        def books(): return {"books": [{"title": "private-book"}]}
        @self.app.get("/api/ai/settings")
        def ai_settings(): return {"api_key": "private-api-secret"}
        @self.app.post("/api/books/book/manga/jobs")
        def generate(): return {"paid_generation": True}
        @self.app.put("/api/books/book/progress")
        def progress(): return {"ok": True}
        @self.app.post("/api/books/book/annotations")
        def annotations(): return {"annotation": {"id": "note"}}
        @self.app.get("/media/book/0")
        def image(): return Response(b"private-image", media_type="image/png", headers={"Cache-Control": "public,max-age=9999"})
        @self.app.get("/api/books/book/manga/exports/e/download")
        def download(): return Response(b"private-cbz", media_type="application/octet-stream")
        self.app.mount("/", StaticFiles(directory=str(config.WEB_DIR), html=True))
        self.local = TestClient(self.app, base_url="http://127.0.0.1:8765", client=("127.0.0.1", 50000))
        self.remote = TestClient(self.app, base_url="http://192.168.1.10:8765", client=("192.168.1.20", 50001))
        self.addCleanup(self.local.close); self.addCleanup(self.remote.close)
        self.origin = {"Origin": "http://192.168.1.10:8765"}

    def pair(self):
        result = self.local.post("/api/security/token", json={})
        self.assertEqual(result.status_code, 200)
        self.token = result.json()["token"]
        response = self.remote.post("/api/auth/login", json={"token": self.token}, headers=self.origin)
        self.assertEqual(response.status_code, 200)
        return response

    def test_default_loopback_only_and_static_login_shell(self):
        self.assertEqual(self.local.get("/api/books").status_code, 200)
        for path in ("/api/books", "/media/book/0", "/api/ai/settings", "/api/books/book/manga/exports/e/download"):
            response = self.remote.get(path)
            self.assertEqual(response.status_code, 403)
            self.assertNotIn("private-", response.text)
        for path in ("/", "/app.js", "/util/core.js", "/api/health", "/api/auth/status"):
            self.assertEqual(self.remote.get(path).status_code, 200)
        self.assertEqual(self.remote.get("/api/auth/status").json(), {"enabled": False, "authenticated": False, "local": False})
        public_state = self.remote.get("/api/security/settings")
        self.assertEqual(public_state.status_code, 200)
        self.assertEqual(public_state.json()["settings"], {"enabled": False, "paired": False, "local": False, "token_configured": False})

    def test_pairing_cookie_covers_books_images_download_and_survives_reload(self):
        response = self.pair()
        cookie = response.headers["set-cookie"].lower()
        self.assertIn("httponly", cookie); self.assertIn("samesite=strict", cookie)
        self.assertNotIn(self.token, cookie)
        for path, body in (("/api/books", "private-book"), ("/media/book/0", "private-image"),
                           ("/api/books/book/manga/exports/e/download", "private-cbz")):
            result = self.remote.get(path)
            self.assertEqual(result.status_code, 200); self.assertIn(body, result.text)
            self.assertIn("private", result.headers["cache-control"]); self.assertIn("no-store", result.headers["cache-control"])
        self.assertTrue(self.remote.get("/api/auth/status").json()["authenticated"])
        persisted = (self.root / "security.json").read_text()
        self.assertNotIn(self.token, persisted)
        self.assertNotIn(self.remote.cookies.get(security.COOKIE_NAME), persisted)
        self.assertNotIn("token_hash", self.local.get("/api/security/settings").text)

    def test_qr_invitation_is_one_use_expiring_and_never_contains_master_code(self):
        with patch("server.app.security._lan_host", return_value="192.168.1.10"):
            result = self.local.post("/api/security/token", json={}).json()
        self.assertTrue(result["pairing_url"].startswith("http://192.168.1.10:8765/#pair="))
        invite = unquote(result["pairing_url"].split("#pair=", 1)[1])
        self.assertNotEqual(invite, result["token"])
        persisted = (self.root / "security.json").read_text()
        self.assertNotIn(invite, persisted); self.assertNotIn(result["token"], persisted)
        png = self.local.post("/api/security/qr", json={"data": result["pairing_url"]})
        self.assertEqual(png.status_code, 200); self.assertEqual(png.headers["content-type"], "image/png")
        self.assertTrue(png.content.startswith(b"\x89PNG"))
        self.assertEqual(self.remote.post("/api/auth/login", json={"token": invite}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.post("/api/auth/login", json={"token": invite}, headers=self.origin).status_code, 401)
        with patch("server.app.security._lan_host", return_value="192.168.1.10"), patch("server.app.security.time.time", return_value=1000):
            result = self.local.post("/api/security/token", json={}).json()
        invite = unquote(result["pairing_url"].split("#pair=", 1)[1])
        with patch("server.app.security.time.time", return_value=1301):
            self.assertEqual(self.remote.post("/api/auth/login", json={"token": invite}, headers=self.origin).status_code, 401)
        self.assertEqual(self.remote.post("/api/security/qr", json={"data": "anything"}, headers=self.origin).status_code, 401)

    def test_paired_readers_cannot_read_credentials_manage_models_or_generate(self):
        self.pair()
        for method, path in (("GET", "/api/ai/settings"), ("GET", "/api/ai/connections"),
                             ("GET", "/api/books/book/manga/config"), ("POST", "/api/books/book/manga/jobs"),
                             ("POST", "/api/security/token"), ("PATCH", "/api/security/settings")):
            response = self.remote.request(method, path, json={}, headers=self.origin)
            self.assertEqual(response.status_code, 403)
            self.assertNotIn("private-api-secret", response.text)
        settings = self.remote.get("/api/security/settings").json()["settings"]
        self.assertTrue(settings["paired"]); self.assertFalse(settings["local"])
        self.assertEqual(self.remote.put("/api/books/book/progress", json={}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.post("/api/books/book/annotations", json={}, headers=self.origin).status_code, 200)

    def test_new_device_invite_preserves_existing_session_and_disable_invalidates_invite(self):
        self.pair()
        invitation = self.local.post('/api/security/invite', json={})
        self.assertEqual(invitation.status_code, 200, invitation.text)
        value = invitation.json()
        self.assertIsNone(value['token'])
        self.assertEqual(self.remote.get('/api/books').status_code, 200)
        self.local.patch('/api/security/settings', json={'enabled': False})
        self.local.patch('/api/security/settings', json={'enabled': True})
        invite = unquote(value['pairing_url'].split('#pair=', 1)[1])
        self.assertEqual(self.remote.post('/api/auth/login', json={'token': invite}, headers=self.origin).status_code, 401)

    def test_origin_checks_cover_local_and_paired_mutations_without_null_bypass(self):
        self.pair()
        for origin in ("https://evil.example", "null", "http://192.168.1.10:9999", "http://192.168.1.10:8765.evil.example"):
            self.assertEqual(self.local.post("/api/security/token", json={}, headers={"Origin": origin}).status_code, 403)
            self.assertEqual(self.remote.put("/api/books/book/progress", json={}, headers={"Origin": origin}).status_code, 403)
        self.assertEqual(self.remote.put("/api/books/book/progress", json={}).status_code, 403)
        self.assertEqual(self.local.post("/api/security/token", json={}, headers={"Sec-Fetch-Site": "cross-site"}).status_code, 403)
        duplicate = [("Origin", "http://192.168.1.10:8765"), ("Origin", "https://evil.example")]
        self.assertEqual(self.remote.put("/api/books/book/progress", json={}, headers=duplicate).status_code, 403)

    def test_rotation_disabling_and_logout_revoke_sessions(self):
        self.pair()
        self.local.post("/api/security/token", json={})
        self.assertEqual(self.remote.get("/api/books").status_code, 401)
        self.assertEqual(self.remote.post("/api/auth/login", json={"token": self.token}, headers=self.origin).status_code, 401)
        self.pair()
        self.assertEqual(self.remote.post("/api/auth/logout", json={}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.get("/api/books").status_code, 401)
        self.pair()
        self.local.patch("/api/security/settings", json={"enabled": False})
        self.assertEqual(self.remote.get("/api/books").status_code, 403)
        self.local.patch("/api/security/settings", json={"enabled": True})
        self.assertEqual(self.remote.get("/api/books").status_code, 401)

    def test_forwarded_headers_and_rebinding_do_not_grant_admin(self):
        spoof = {"X-Forwarded-For": "127.0.0.1", "X-Real-IP": "127.0.0.1", "Forwarded": "for=127.0.0.1"}
        self.assertEqual(self.remote.get("/api/ai/settings", headers=spoof).status_code, 403)
        with TestClient(self.app, base_url="http://evil.example", client=("127.0.0.1", 50000)) as rebound:
            self.assertEqual(rebound.get("/api/ai/settings").status_code, 403)
            self.assertFalse(rebound.get("/api/auth/status").json()["local"])
        self.pair()
        self.assertEqual(self.remote.get("/api/ai/settings", headers=spoof).status_code, 403)
        for path in ("/data/security.json", "/docs", "/openapi.json", "/ui/%2e%2e/api/ai/settings", "/ui/%5c../security.json"):
            self.assertNotEqual(self.remote.get(path).status_code, 200)

    def test_invalid_file_and_login_throttling_fail_closed(self):
        self.pair()
        for _ in range(12):
            self.assertEqual(self.remote.post("/api/auth/login", json={"token": "wrong"}, headers=self.origin).status_code, 401)
        self.assertEqual(self.remote.post("/api/auth/login", json={"token": self.token}, headers=self.origin).status_code, 401)
        (self.root / "security.json").write_text("invalid-json")
        self.assertEqual(self.remote.get("/api/books").status_code, 403)
        self.assertEqual(self.local.get("/api/books").status_code, 200)

    def test_real_application_media_annotations_source_and_progress(self):
        from server.app.main import create_app
        with patch("server.app.db.DB_PATH", self.root / "app.db"):
            app = create_app()
            with TestClient(app, base_url="http://127.0.0.1:8765", client=("127.0.0.1", 50100)) as local, \
                    TestClient(app, base_url="http://192.168.1.10:8765", client=("192.168.1.20", 50101)) as remote:
                text = "第一章 测试\n\n这是局域网配对测试正文。".encode()
                imported = local.post("/api/books?filename=pairing.txt", content=text)
                self.assertEqual(imported.status_code, 201)
                book = imported.json()["book"]["id"]
                png = io.BytesIO(); Image.new("RGB", (64, 64), "white").save(png, "PNG")
                archive = io.BytesIO()
                with zipfile.ZipFile(archive, "w") as bundle:
                    bundle.writestr("1.png", png.getvalue())
                comic = local.post("/api/books?filename=pairing.cbz", content=archive.getvalue()).json()["book"]["id"]
                self.assertEqual(remote.get(f"/media/{comic}/0").status_code, 403)
                token = local.post("/api/security/token", json={}).json()["token"]
                self.assertEqual(remote.post("/api/auth/login", json={"token": token}, headers=self.origin).status_code, 200)
                media = remote.get(f"/media/{comic}/0")
                self.assertEqual(media.content, png.getvalue())
                self.assertEqual(media.headers["cache-control"], "private, no-store")
                self.assertEqual(remote.get(f"/api/books/{book}/source").content, text)
                para = remote.get(f"/api/books/{book}/chapters/0").json()["paragraphs"][0]
                note = remote.post(f"/api/books/{book}/annotations", json={"chapter": 0, "para": 0,
                    "start_offset": 0, "end_offset": 2, "text": para[:2], "note": "paired note"}, headers=self.origin)
                self.assertEqual(note.status_code, 201)
                annotation = note.json()["annotation"]["id"]
                self.assertEqual(remote.patch(f"/api/books/{book}/annotations/{annotation}", json={"note": "edited"}, headers=self.origin).status_code, 200)
                self.assertEqual(remote.put(f"/api/books/{book}/progress", json={"chapter": 0, "para": 0, "offset": 2}, headers=self.origin).status_code, 200)
                self.assertEqual(remote.get(f"/api/books/{book}/progress").json()["progress"]["offset"], 2)
                self.assertEqual(remote.delete(f"/api/books/{book}/annotations/{annotation}", headers=self.origin).status_code, 200)
                self.assertEqual(remote.get("/api/ai/catalog").status_code, 403)


if __name__ == "__main__":
    unittest.main()
