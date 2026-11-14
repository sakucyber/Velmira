"""Plot pagination uses sentence indices while preserving every source byte."""
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from server.app import db
from server.app.manga import pagination as p
from server.app.manga.models import MangaError
from manga_fixture import seed


class PaginationTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        for target, value in [("server.app.db.DB_PATH", self.root / "app.db"),
                              ("server.app.config.DATA_DIR", self.root),
                              ("server.app.ai.retry.RETRY_DELAY", 0)]:
            mocked = patch(target, value); mocked.start(); self.addCleanup(mocked.stop)
        db.init_db(); seed()
        self.text = "  林澄走进房间。她看见一把钥匙。\n她拿起钥匙。门打开了。\n\n她走了出去。 "
        conn = db.get_conn()
        with conn: conn.execute("UPDATE segments SET text=? WHERE id='m3-seg'", (self.text,))
        conn.close()

    def pages(self):
        return [{"start_sentence": 0, "end_sentence": 1, "panels": 3, "layout": "1+2"},
                {"start_sentence": 2, "end_sentence": 4, "panels": 5, "layout": "1+2+2"}]

    def test_sentences_preserve_character_locations(self):
        items = p.sentences(self.text)
        self.assertEqual(len(items), 5)
        self.assertEqual([self.text[x["start"]:x["end"]] for x in items], [x["text"] for x in items])

    def test_multiple_pages_cover_every_character_once(self):
        items = p.sentences(self.text)
        pages = p.normalize({"pages": self.pages()}, self.text, items)
        self.assertEqual("".join(self.text[x["start"]:x["end"]] for x in pages), self.text)
        self.assertEqual(pages[0]["end"], pages[1]["start"])
        self.assertEqual([x["panels"] for x in pages], [3, 5])

    def test_reject_missing_duplicate_out_of_order_and_eight_panels(self):
        invalid = [
            [{"start_sentence": 1, "end_sentence": 4, "panels": 1}],
            [{"start_sentence": 0, "end_sentence": 2, "panels": 1}],
            [{"start_sentence": 0, "end_sentence": 2, "panels": 1}, {"start_sentence": 2, "end_sentence": 4, "panels": 1}],
            [{"start_sentence": 3, "end_sentence": 4, "panels": 1}, {"start_sentence": 0, "end_sentence": 2, "panels": 1}],
            [{"start_sentence": 0, "end_sentence": 4, "panels": 8}],
            [{"start_sentence": 0, "end_sentence": 4, "panels": 3, "layout": "2x2"}],
        ]
        for pages in invalid:
            with self.subTest(pages=pages), self.assertRaises(MangaError):
                p.normalize({"pages": pages}, self.text, p.sentences(self.text))

    def test_plan_cache_avoids_repeat_model_calls(self):
        provider = SimpleNamespace(complete=AsyncMock(return_value={"pages": self.pages()}))
        with patch.object(p.director, "text_config", return_value={"provider": "mock", "model": "test"}), patch.object(p, "make_provider", return_value=provider):
            first = asyncio.run(p.plan("m3-book", "m3-scene"))
            second = asyncio.run(p.plan("m3-book", "m3-scene"))
        self.assertEqual(first, second)
        self.assertEqual(provider.complete.await_count, 1)

    def test_single_short_sentence_uses_one_page_without_model(self):
        conn = db.get_conn()
        with conn: conn.execute("UPDATE segments SET text='她笑了。' WHERE id='m3-seg'")
        conn.close()
        with patch.object(p.director, "text_config", return_value={"provider": "mock", "model": "test"}), patch.object(p, "make_provider") as make:
            pages = asyncio.run(p.plan("m3-book", "m3-scene"))
        make.assert_not_called()
        self.assertEqual([(x["start"], x["end"], x["panels"]) for x in pages], [(0, 4, 1)])


if __name__ == "__main__": unittest.main()
