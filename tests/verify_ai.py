#!/usr/bin/env python
"""M2 离线验收：规则分段 → Mock scene → 人物卡冻结/完整性。"""
from __future__ import annotations

import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="velmira-m2-") as data_dir:
        os.environ["DATA_DIR"] = data_dir
        from server.app.db import get_conn, init_db

        init_db()
        conn = get_conn()
        conn.execute(
            "INSERT INTO books(id,title,file_path,sha256,created_at) VALUES(?,?,?,?,?)",
            ("m2-book", "M2 sample", "sample.txt", "m2-sha", 1),
        )
        paragraphs = [
            "林青走进房间，黑色短发的林青说道：你好。",
            "夜里，林青拿起钥匙走向门口。",
            "林青回头看见朋友，点头走来。",
        ]
        conn.execute(
            "INSERT INTO chapters(id,book_id,idx,title,chars,paras_json) VALUES(?,?,?,?,?,?)",
            ("m2-ch0", "m2-book", 0, "第一章", sum(map(len, paragraphs)), json.dumps(paragraphs, ensure_ascii=False)),
        )
        conn.commit()
        conn.close()

        from fastapi.testclient import TestClient
        from server.app.main import app

        client = TestClient(app)
        response = client.post("/api/books/m2-book/segment", json={"per_chapter": 1, "params": {"min_chars": 1}})
        assert response.status_code == 200, response.text
        response = client.post("/api/books/m2-book/ai/extract", json={"provider": "mock"})
        assert response.status_code == 200, response.text
        result = response.json()
        assert result["scenes"] == 1 and result["needs_review"] == 0, result
        scenes = client.get("/api/books/m2-book/ai/scenes").json()["scenes"]
        chars = client.get("/api/books/m2-book/ai/characters").json()["characters"]
        assert scenes and chars
        character = next(item for item in chars if item["name"] == "林青")
        assert all(e["quote"] in "\n".join(paragraphs) for e in character["card"]["evidence"])
        frozen = client.post(f"/api/books/m2-book/ai/characters/{character['id']}/freeze", json={})
        assert frozen.status_code == 200, frozen.text
        frozen_character = frozen.json()["character"]
        assert frozen_character["frozen"] and frozen_character["identity_hash"]
        blocked = client.patch(
            f"/api/books/m2-book/ai/characters/{character['id']}",
            json={"card": {"hair_color": "red"}},
        )
        assert blocked.status_code == 409 and blocked.json()["error"]["code"] == "character_frozen", blocked.text
        cached = client.post("/api/books/m2-book/ai/extract", json={"provider": "mock"})
        assert cached.status_code == 200 and cached.json()["cached"] >= 1, cached.text
        print(json.dumps({"scenes": len(scenes), "characters": len(chars), "identity_frozen": True, "cache_hits": cached.json()["cached"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
