"""Source locators and page ranges stay reproducible across memory updates."""
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch
from contextlib import nullcontext

from server.app import db
from server.app.manga import storyboard as b
from server.app.manga import pages as page_service
from server.app.manga.models import MangaError
from manga_fixture import seed


TEXT = "甲走进房间。乙打开窗户。甲拿起钥匙。乙转身微笑。甲推开房门。乙挥手道别。甲离开房间。"


def panel(quote, start=None):
    value = {"kind": "environment", "shot": "wide", "subjects": [], "action": "a quiet room",
             "environment": "a room with a window", "lighting": "", "emotion": "", "caption": "",
             "dialogue": "", "speaker": "", "source_quote": quote}
    if start is not None:
        value.update(source_start=start, source_end=start + len(quote))
    return value


class StoryboardSourceTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"),
                              ("server.app.config.DATA_DIR", self.root),
                              ("server.app.ai.retry.RETRY_DELAY", 0)]:
            mocked = patch(target, value); mocked.start(); self.addCleanup(mocked.stop)
        db.init_db(); seed()
        conn = db.get_conn()
        with conn:
            conn.execute("UPDATE segments SET text=? WHERE id='m3-seg'", (TEXT,))
            conn.execute("UPDATE chapters SET paras_json=? WHERE id='m3-ch'", (json.dumps([TEXT]),))
        conn.close()
        self.cfg = {"provider": "mock", "base_url": "http://fixture", "model": "fixture"}
        mocked = patch.object(b.director, "text_config", return_value=self.cfg); mocked.start(); self.addCleanup(mocked.stop)

    def material(self):
        conn = db.get_conn()
        try: return b.material(conn, "m3-book", "m3-scene")
        finally: conn.close()

    def test_seven_panels_accept_and_eight_reject(self):
        source = self.material()
        quote = "甲走进房间。"
        accepted = b.normalize({"panels": [panel(quote)] * 7}, source)
        self.assertEqual(len(accepted), 7)
        with self.assertRaises(MangaError): b.normalize({"panels": [panel(quote)] * 8}, source)

    def test_precise_repeated_quote_range_and_mismatched_offsets(self):
        text = "他笑了。她说话。他笑了。"
        source = {"text": text, "characters": [], "chapter": 2, "segment_start": 50}
        start = text.rfind("他笑了。")
        normalized = b.normalize({"panels": [panel("他笑了。", start)]}, source)[0]
        self.assertEqual(b.source_span(source, normalized), {"chapter": 2, "start": 50 + start, "end": 50 + start + 4, "quote": "他笑了。"})
        invalid = panel("她说话。", 0)
        with self.assertRaises(MangaError): b.normalize({"panels": [invalid]}, source)

    def test_source_quote_never_enters_image_prompt(self):
        quote = "这是定位原文不是画面提示词。" * 500
        value = panel(quote)
        value["source_span"] = {"chapter": 0, "start": 0, "end": len(quote), "quote": quote}
        prompt = b.prompt(value, [], "anime style")
        self.assertNotIn(quote, prompt)
        self.assertNotIn("这是定位原文", prompt)
        self.assertIn("a quiet room", prompt)

    def test_multi_page_create_sees_only_its_slice(self):
        split = TEXT.index("甲拿起钥匙。")
        ranges = [{"start": 0, "end": split, "panels": 2, "layout": "2x1"},
                  {"start": split, "end": len(TEXT), "panels": 5, "layout": "1+2+2"}]
        seen = []
        async def complete(**kwargs):
            source = json.loads(kwargs["user"])
            seen.append(source["text"])
            return {"panels": [{**panel(source["text"]), "start_sentence": 0, "end_sentence": len(source["sentences"]) - 1}]}
        provider = SimpleNamespace(complete=AsyncMock(side_effect=complete))
        with patch.object(b, "make_provider", return_value=provider):
            first = asyncio.run(b.create("m3-book", "m3-scene", page_range=ranges[0]))
            second = asyncio.run(b.create("m3-book", "m3-scene", page_range=ranges[1]))
        self.assertNotEqual(first["id"], second["id"])
        self.assertEqual(seen, [TEXT[:split], TEXT[split:]])
        self.assertEqual(second["material"]["segment_start"], split)

    def test_memory_changes_do_not_stale_but_source_changes_do(self):
        provider = SimpleNamespace(complete=AsyncMock(return_value={"panels": [{**panel("甲走进房间。"), "start_sentence": 0, "end_sentence": 6}]}))
        with patch.object(b, "make_provider", return_value=provider):
            board = asyncio.run(b.create("m3-book", "m3-scene"))
        conn = db.get_conn()
        try:
            original = b.material
            def changed(*args, **kwargs):
                source = original(*args, **kwargs)
                source["long_term_memory"] = [{"content": "a newly saved historical note"}]
                source["chapter_memory"] = {"version": 2, "facts": []}
                return source
            with patch.object(b, "material", side_effect=changed):
                self.assertEqual(b.get(conn, "m3-book", board["id"])["id"], board["id"])
            with conn: conn.execute("UPDATE segments SET text=? WHERE id='m3-seg'", (TEXT + "天亮了。",))
            with self.assertRaises(MangaError): b.get(conn, "m3-book", board["id"])
        finally: conn.close()

    def test_auto_assembly_preserves_two_boards_in_one_scene(self):
        # A director may split one long analysis scene into two planned pages.
        # Auto assembly must preserve those page boundaries even though seven
        # images would fit together in its fallback maximum-size layout.
        rows = [{"job_id": "job-" + str(i), "chapter": 0, "para": i,
                 "board_id": "first-board" if i < 2 else "second-board"}
                for i in range(7)]
        by_id = {row["job_id"]: row for row in rows}
        def captured(_conn, _book, spec):
            return [by_id[item["job_id"]] for item in spec["panels"]]
        with patch.object(page_service, "connection", return_value=nullcontext(None)), \
                patch.object(page_service, "sources", side_effect=captured), \
                patch.object(page_service, "create_pages", side_effect=lambda _book, specs: specs):
            specs = page_service.auto_pages("m3-book", {"job_ids": list(by_id)})
        self.assertEqual([len(spec["panels"]) for spec in specs], [2, 5])
        self.assertTrue(all(len(spec["panels"]) <= 7 for spec in specs))
        self.assertEqual([slot["job_id"] for spec in specs for slot in spec["panels"]], list(by_id))

    def test_sentence_groups_cover_source_and_expand_quotes_to_full_group(self):
        source = {"text":"甲开门。乙取钥匙。丙走进来。", "characters":[], "chapter":0, "segment_start":10}
        values = [{**panel("甲开门。"), "start_sentence":0, "end_sentence":1},
                  {**panel("丙走进来。"), "start_sentence":2, "end_sentence":2}]
        panels = b.normalize({"panels":values}, source)
        self.assertEqual([p["source_quote"] for p in panels], ["甲开门。乙取钥匙。", "丙走进来。"])
        spans=[b.source_span(source,p) for p in panels]
        self.assertEqual(spans[0]["start"],10)
        self.assertEqual(spans[0]["end"],spans[1]["start"])
        self.assertEqual(spans[1]["end"],10+len(source["text"]))
        self.assertEqual("".join(p["source_quote"] for p in panels),source["text"])

    def test_sentence_groups_reject_skips_overlaps_missing_tail_and_invalid_bounds(self):
        source={"text":"甲开门。乙取钥匙。丙走进来。", "characters":[], "chapter":0, "segment_start":0}
        def group(first,last,quote): return {**panel(quote),"start_sentence":first,"end_sentence":last}
        invalid = [
            [group(0,0,"甲开门。"),group(2,2,"丙走进来。")],
            [group(0,1,"甲开门。"),group(1,2,"乙取钥匙。")],
            [group(0,1,"甲开门。")],
            [group(1,2,"乙取钥匙。")],
            [group(0,3,"甲开门。")],
            [group(False,2,"甲开门。")],
            [group(0,-1,"甲开门。")],
            [group(0,0,"丙走进来。"),group(1,2,"乙取钥匙。")],
        ]
        for values in invalid:
            with self.subTest(values=values),self.assertRaises(MangaError):
                b.normalize({"panels":values},source)

    def test_repeated_exact_sentences_map_to_distinct_ordered_groups(self):
        source={"text":"他笑了。他笑了。他笑了。", "characters":[], "chapter":2, "segment_start":50}
        values=[{**panel("他笑了。"),"start_sentence":i,"end_sentence":i} for i in range(3)]
        panels=b.normalize({"panels":values},source)
        self.assertEqual([(p["source_start"],p["source_end"]) for p in panels],[(0,4),(4,8),(8,12)])
        self.assertEqual([(b.source_span(source,p)["start"],b.source_span(source,p)["end"]) for p in panels],[(50,54),(54,58),(58,62)])

    def test_sentence_group_offsets_preserve_emoji_whitespace_and_half_open_ranges(self):
        text="甲🙂招手。\n乙😀笑了。 甲走开。"
        source={"text":text,"characters":[],"chapter":1,"segment_start":100}
        values=[{**panel(quote),"start_sentence":i,"end_sentence":i} for i,quote in enumerate(["甲🙂招手。","乙😀笑了。","甲走开。"]) ]
        panels=b.normalize({"panels":values},source)
        self.assertEqual("".join(p["source_quote"] for p in panels),text)
        self.assertEqual(panels[0]["source_end"],text.index("乙"))
        self.assertEqual(panels[1]["source_end"],text.index("甲走开"))
        self.assertEqual(b.source_span(source,panels[-1])["end"],100+len(text))


if __name__ == "__main__": unittest.main()
