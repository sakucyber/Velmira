"""Resumable synchronization and paired-device security, with isolated SQLite."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fastapi import FastAPI
from fastapi.testclient import TestClient

from server.app import db, importer, security, sync
from server.app.api.sync import router as sync_router
from server.app.api.security import router as security_router


class SyncTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in (("server.app.config.DATA_DIR", self.root), ("server.app.db.DB_PATH", self.root / "app.db"), ("server.app.security._login_attempts", {})):
            item = patch(target, value); item.start(); self.addCleanup(item.stop)
        db.init_db()
        self.book = importer.import_book("第一章 测试\n\n这是原文段落，可以用来验证同步。\n\n第二段内容。".encode(), "sync.txt")["book"]["id"]
        conn = db.get_conn(); self.para = __import__("json").loads(conn.execute("SELECT paras_json FROM chapters WHERE book_id=? AND idx=0", (self.book,)).fetchone()[0])[0]; conn.close()
        self.device = sync.register_device({"device_id": "phone", "label": "手机"})["device"]["id"]
        self.app = FastAPI(); security.install_security(self.app)
        self.app.include_router(security_router); self.app.include_router(sync_router)
        self.local = TestClient(self.app, base_url="http://127.0.0.1:8765", client=("127.0.0.1", 1)); self.addCleanup(self.local.close)
        self.remote = TestClient(self.app, base_url="http://192.168.1.1:8765", client=("192.168.1.2", 2)); self.addCleanup(self.remote.close)
        self.origin = {"Origin": "http://192.168.1.1:8765"}

    def op(self, entity="progress", entity_id=None, seq=1, action="upsert", payload=None, **extra):
        if payload is None: payload = {"chapter": 0, "para": 0, "offset": 2, "percent": .1, "updated_at": 100}
        return {"op_id": f"op-{seq}", "seq": seq, "entity": entity, "entity_id": entity_id or self.book, "action": action, "payload": payload, **extra}

    def annotation(self, seq=1, revision=0, note="本机草稿"):
        return self.op("annotation", "ann-offline", seq, payload={"book_id": self.book, "chapter": 0, "para": 0, "start_offset": 0, "end_offset": 2, "text": self.para[:2], "note": note, "color": "yellow", "updated_at": 100 + seq}, base_revision=revision)

    def test_device_registration_reuses_identity_and_reports_sequence(self):
        sync.push(self.device, [self.op()])
        device = sync.register_device({"device_id": "phone", "label": "Phone"})["device"]
        self.assertEqual(device["last_seq"], 1)
        self.assertEqual(device["label"], "Phone")
        self.assertEqual(self.local.delete("/api/sync/devices/phone").status_code, 200)
        with self.assertRaises(sync.SyncError): sync.pull("phone")

    def test_existing_library_initialization_adds_sync_without_resetting_data(self):
        conn = db.get_conn()
        with conn:
            for table in ("sync_device_sessions", "sync_entity_states", "sync_operations", "sync_devices"):
                conn.execute("DROP TABLE " + table)
            conn.execute("INSERT INTO progress(book_id,chapter,para,offset,percent,updated_at) VALUES(?,0,0,1,.1,50)", (self.book,))
        conn.close()
        db.init_db(); db.init_db()
        snapshot = sync.snapshot(self.book)
        self.assertEqual(snapshot["books"][0]["id"], self.book)
        self.assertEqual(snapshot["progress"][0]["offset"], 1)
        self.assertGreater(snapshot["progress"][0]["sync_revision"], 0)

    def test_push_is_idempotent_and_sequence_identity_is_strict(self):
        result = sync.push(self.device, [self.op()]); self.assertEqual(len(result["accepted"]), 1)
        duplicate = sync.push(self.device, [self.op()]); self.assertEqual(duplicate["duplicates"], ["op-1"])
        altered = self.op(); altered["op_id"] = "other"
        with self.assertRaises(sync.SyncError) as error: sync.push(self.device, [altered])
        self.assertEqual(error.exception.status, 409)
        conn = db.get_conn(); self.assertEqual(conn.execute("SELECT COUNT(*) FROM sync_operations WHERE device_id='phone'").fetchone()[0], 1); conn.close()

    def test_progress_percent_is_derived_from_location_without_breaking_retries(self):
        operation = self.op()
        operation["payload"]["percent"] = .99
        sync.push(self.device, [operation])
        conn = db.get_conn()
        total = conn.execute("SELECT SUM(chars) FROM chapters WHERE book_id=?", (self.book,)).fetchone()[0]
        persisted = conn.execute("SELECT percent FROM progress WHERE book_id=?", (self.book,)).fetchone()[0]
        conn.close()
        expected = round(2 / total, 6)
        self.assertEqual(persisted, expected)
        self.assertEqual(sync.snapshot(self.book)["progress"][0]["percent"], expected)
        self.assertEqual(sync.pull(self.device)["operations"][0]["payload"]["percent"], expected)
        self.assertEqual(sync.push(self.device, [operation])["duplicates"], [operation["op_id"]])

    def test_cbz_progress_uses_page_count_even_with_stale_client_percent(self):
        conn = db.get_conn()
        with conn:
            conn.execute("UPDATE books SET format='cbz',chapter_count=5 WHERE id=?", (self.book,))
        conn.close()
        operation = self.op(payload={"chapter": 2, "para": 0, "offset": 0, "percent": 0, "updated_at": 100})
        sync.push(self.device, [operation])
        self.assertEqual(sync.snapshot(self.book)["progress"][0]["percent"], .6)
        self.assertEqual(sync.pull(self.device)["operations"][0]["payload"]["percent"], .6)
        self.assertEqual(sync.push(self.device, [operation])["duplicates"], [operation["op_id"]])

    def test_future_clock_conflict_can_explicitly_keep_local_progress_and_annotation(self):
        future = 4_000_000_000
        progress = self.op(); progress["payload"]["updated_at"] = future
        sync.push(self.device, [progress])
        draft = self.op(seq=2)
        conflict = sync.push(self.device, [draft])["conflicts"][0]
        self.assertEqual(conflict["reason"], "server_newer")
        self.assertEqual(conflict["current_updated_at"], future)
        draft["op_id"] = "resolved-progress"; draft["seq"] = 3
        draft["payload"]["updated_at"] = conflict["current_updated_at"] + 1
        self.assertEqual(len(sync.push(self.device, [draft])["accepted"]), 1)

        annotation = self.annotation(seq=4); annotation["payload"]["updated_at"] = future
        revision = sync.push(self.device, [annotation])["accepted"][0]["cursor"]
        draft = self.annotation(seq=5, revision=revision, note="明确保留本机")
        conflict = sync.push(self.device, [draft])["conflicts"][0]
        self.assertEqual(conflict["reason"], "server_newer")
        self.assertEqual(conflict["current_updated_at"], future)
        stale = self.annotation(seq=6)
        stale_conflict = sync.push(self.device, [stale])["conflicts"][0]
        self.assertEqual(stale_conflict["reason"], "revision_conflict")
        self.assertEqual(stale_conflict["current_updated_at"], future)
        draft["op_id"] = "resolved-annotation"; draft["seq"] = 7
        draft["payload"]["updated_at"] = conflict["current_updated_at"] + 1
        self.assertEqual(len(sync.push(self.device, [draft])["accepted"]), 1)
        snapshot = sync.snapshot(self.book)
        self.assertEqual(snapshot["progress"][0]["updated_at"], future + 1)
        self.assertEqual(snapshot["annotations"][0]["note"], "明确保留本机")

    def test_pull_cursor_pagination_and_server_rest_changes(self):
        sync.push(self.device, [self.op()])
        initial = sync.pull(self.device, limit=1); self.assertEqual(len(initial["operations"]), 1)
        conn = db.get_conn(); conn.execute("UPDATE progress SET offset=3,updated_at=200 WHERE book_id=?", (self.book,)); conn.commit(); conn.close()
        changed = sync.pull(self.device, initial["cursor"])
        self.assertEqual(changed["operations"][0]["device_id"], "server")
        self.assertEqual(changed["operations"][0]["payload"]["offset"], 3)
        self.assertEqual(sync.pull(self.device, changed["cursor"])["operations"], [])

    def test_annotation_conflict_keeps_both_drafts_outside_server_overwrite(self):
        first = sync.push(self.device, [self.annotation()]); revision = first["accepted"][0]["cursor"]
        conflict = sync.push(self.device, [self.annotation(seq=2, note="旧设备修改")])
        self.assertEqual(conflict["conflicts"][0]["revision"], revision)
        self.assertEqual(conflict["accepted"], [])
        self.assertEqual(sync.snapshot()["annotations"][0]["note"], "本机草稿")
        updated = sync.push(self.device, [self.annotation(seq=3, revision=revision, note="已复核")])
        self.assertEqual(len(updated["accepted"]), 1)
        snapshot = sync.snapshot(); self.assertEqual(snapshot["annotations"][0]["note"], "已复核")
        self.assertEqual(snapshot["annotations"][0]["sync_revision"], updated["accepted"][0]["cursor"])

    def test_snapshot_projects_public_book_metadata_and_no_credentials(self):
        sync.push(self.device, [self.op()])
        snapshot = sync.snapshot(); self.assertEqual(snapshot["books"][0]["id"], self.book)
        self.assertNotIn("file_path", snapshot["books"][0]); self.assertNotIn("memories", snapshot)
        self.assertEqual(snapshot["progress"][0]["sync_revision"], snapshot["cursor"])
        manifest = self.local.get(f"/api/sync/books/{self.book}/manifest")
        self.assertEqual(manifest.status_code, 200)
        self.assertEqual(len(manifest.json()["resources"]), manifest.json()["book"]["chapter_count"])
        self.assertNotIn("file_path", str(manifest.json()))

    def test_snapshot_pages_all_arrays_and_keeps_resume_cursor(self):
        sync.push(self.device, [self.annotation(seq=1)])
        second = self.annotation(seq=2); second["entity_id"] = "ann-second"
        sync.push(self.device, [second])
        first_page = sync.snapshot(self.book, limit=1)
        self.assertTrue(first_page["has_more"])
        self.assertEqual(len(first_page["annotations"]), 1)
        next_page = sync.snapshot(self.book, offset=first_page["next_offset"], limit=1)
        self.assertFalse(next_page["has_more"])
        self.assertEqual(len(next_page["annotations"]), 1)
        self.assertNotEqual(first_page["annotations"][0]["id"], next_page["annotations"][0]["id"])
        self.assertEqual(first_page["cursor"], next_page["cursor"])

    def test_regular_api_edit_is_conflict_against_old_offline_annotation(self):
        accepted = sync.push(self.device, [self.annotation()])["accepted"][0]
        conn = db.get_conn(); conn.execute("UPDATE annotations SET note='服务端后来修改',updated_at=999 WHERE id='ann-offline'"); conn.commit(); conn.close()
        draft = self.annotation(seq=2, revision=accepted["cursor"], note="离线旧副本")
        result = sync.push(self.device, [draft])
        self.assertEqual(result["conflicts"][0]["reason"], "revision_conflict")
        self.assertEqual(sync.snapshot()["annotations"][0]["note"], "服务端后来修改")

    def test_invalid_fields_coordinates_and_batch_are_rolled_back(self):
        for change in ({"offset": 9999}, {"api_key": "secret"}, {"chapter": -1}):
            operation = self.op(); operation["payload"].update(change)
            with self.assertRaises(sync.SyncError): sync.push(self.device, [operation])
        annotation = self.annotation(); annotation["payload"]["color"] = "red"
        with self.assertRaises(sync.SyncError): sync.push(self.device, [annotation])
        valid = self.op(); invalid = self.op(seq=2); invalid["payload"]["offset"] = 9999
        with self.assertRaises(sync.SyncError): sync.push(self.device, [valid, invalid])
        self.assertEqual(sync.snapshot()["progress"], [])

    def test_deleted_bookmark_tombstone_prevents_stale_resurrection(self):
        payload = {"book_id": self.book, "chapter": 0, "para": 0, "offset": 0, "label": "书签", "created_at": 1}
        sync.push(self.device, [self.op("bookmark", "bm-offline", 1, payload=payload)])
        sync.push(self.device, [self.op("bookmark", "bm-offline", 2, action="delete", payload={})])
        result = sync.push(self.device, [self.op("bookmark", "bm-offline", 3, payload=payload)])
        self.assertEqual(result["conflicts"][0]["reason"], "deleted")
        self.assertEqual(sync.snapshot()["bookmarks"], [])

    def test_duplicate_bookmark_does_not_log_nonexistent_entity(self):
        payload = {"book_id": self.book, "chapter": 0, "para": 0, "offset": 0, "label": "书签", "created_at": 1}
        sync.push(self.device, [self.op("bookmark", "bm-first", 1, payload=payload)])
        result = sync.push(self.device, [self.op("bookmark", "bm-second", 2, payload=payload)])
        self.assertEqual(result["conflicts"][0]["canonical_id"], "bm-first")
        self.assertEqual(len(sync.snapshot()["bookmarks"]), 1)
        self.assertFalse(any(item["entity_id"] == "bm-second" for item in sync.pull(self.device)["operations"]))

    def test_pairing_is_required_and_remote_cannot_sync_memories(self):
        self.assertEqual(self.remote.get("/api/sync/snapshot").status_code, 403)
        token = self.local.post("/api/security/token", json={}).json()["token"]
        self.assertEqual(self.remote.post("/api/auth/login", json={"token": token}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.post("/api/sync/push", json={"device_id": "tablet", "operations": [self.op()]}, headers=self.origin).status_code, 200)
        memory = self.op("memory", "mem-1", 2, payload={"title": "人物人格", "content": "秘密人格设定"})
        response = self.remote.post("/api/sync/push", json={"device_id": "tablet", "operations": [memory]}, headers=self.origin)
        self.assertEqual(response.status_code, 403)
        sync.push("phone", [memory])
        remote = self.remote.get("/api/sync/pull?device_id=tablet").json()
        self.assertNotIn("秘密人格设定", str(remote))
        self.assertEqual(self.remote.get("/api/sync/devices").status_code, 403)
        self.assertEqual(self.remote.post("/api/sync/push", json={"device_id": "tablet", "operations": [self.op(seq=3)]}).status_code, 403)

    def test_device_is_bound_to_pairing_cookie_and_revoke_only_its_session(self):
        token = self.local.post("/api/security/token", json={}).json()["token"]
        self.remote.post("/api/auth/login", json={"token": token}, headers=self.origin)
        self.assertEqual(self.remote.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin).status_code, 200)
        other = TestClient(self.app, base_url="http://192.168.1.1:8765", client=("192.168.1.3", 3)); self.addCleanup(other.close)
        other.post("/api/auth/login", json={"token": token}, headers=self.origin)
        self.assertEqual(other.get("/api/sync/pull?device_id=tablet").status_code, 403)
        self.assertEqual(other.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin).status_code, 403)
        self.assertEqual(other.post("/api/sync/device", json={"device_id": "other"}, headers=self.origin).status_code, 200)
        self.assertEqual(self.local.delete("/api/sync/devices/phone").status_code, 200)
        self.assertEqual(other.get("/api/sync/snapshot").status_code, 200)
        self.assertEqual(self.local.delete("/api/sync/devices/tablet").status_code, 200)
        self.assertEqual(self.remote.get("/api/sync/snapshot").status_code, 401)
        self.assertEqual(other.get("/api/sync/snapshot").status_code, 200)
        self.assertEqual(other.get("/api/sync/pull?device_id=other").status_code, 200)

    def test_new_pairing_can_rebind_expired_owner_but_not_revoked_device(self):
        token = self.local.post("/api/security/token", json={}).json()["token"]
        self.remote.post("/api/auth/login", json={"token": token}, headers=self.origin)
        self.remote.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin)
        self.assertEqual(self.remote.post("/api/auth/logout", json={}, headers=self.origin).status_code, 200)
        self.remote.post("/api/auth/login", json={"token": token}, headers=self.origin)
        self.assertEqual(self.remote.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin).status_code, 200)
        self.assertEqual(self.remote.get("/api/sync/pull?device_id=tablet").status_code, 200)
        self.local.delete("/api/sync/devices/tablet")
        self.remote.post("/api/auth/login", json={"token": token}, headers=self.origin)
        response = self.remote.post("/api/sync/device", json={"device_id": "tablet"}, headers=self.origin)
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["error"]["code"], "device_revoked")


if __name__ == "__main__": unittest.main()
