"""M2 核心回归：离线 provider、证据过滤、人物卡合并与冻结。"""
from __future__ import annotations

import asyncio
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from server.app.ai.models import AIModelError, identity_hash, normalize_scene, render_identity
from server.app.ai.providers import MockTextProvider
from server.app.ai.extraction import extract_one


class AIExtractionTests(unittest.TestCase):
    def test_mock_provider_is_deterministic_and_schema_safe(self):
        segment = {"id": "c0-s0", "chapter": 0, "text": "林青走进房间，黑色短发的林青说道：“你好。”"}
        first = asyncio.run(extract_one(MockTextProvider(), segment=segment, prompt_version=1))
        second = asyncio.run(extract_one(MockTextProvider(), segment=segment, prompt_version=1))
        self.assertEqual(first, second)
        self.assertEqual(first["kind"], "scene")
        self.assertEqual(first["location"], "房间")
        self.assertTrue(first["characters"])
        self.assertTrue(all(e["quote"] in segment["text"] for c in first["characters"] for e in c["evidence"]))


    def test_invalid_evidence_is_dropped(self):
        value = {
            "kind": "scene",
            "summary": "a",
            "characters": [{
                "name": "A",
                "card": {"hair_color": "blue"},
                "evidence": [{"field": "hair_color", "value": "blue", "quote": "missing", "ch": 0}],
            }],
        }
        result = normalize_scene(value, "A enters.", 0)
        self.assertEqual(result["characters"][0]["card"], {})
        self.assertEqual(result["characters"][0]["evidence"], [])


    def test_invalid_scene_kind_is_rejected(self):
        with self.assertRaises(AIModelError):
            normalize_scene({"kind": "prompt"}, "text", 0)


    def test_identity_block_order_and_hash_are_stable(self):
        card = {
            "accessories": "silver ring",
            "outfit": "school uniform",
            "skin": "fair",
            "eye_color": "blue",
            "eye_shape": "round",
            "hair_color": "black",
            "hair_style": "short",
            "gender": "girl",
            "age": "17",
        }
        block = render_identity(card, "A")
        self.assertEqual(block, "A, 17-year-old girl, short black hair, round blue eyes, fair skin, wearing school uniform, with silver ring")
        self.assertEqual(identity_hash(block), identity_hash(block))


if __name__ == "__main__":
    unittest.main()
