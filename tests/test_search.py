"""检索索引单元测试（unittest，临时目录 + sqlite，无第三方依赖）。

运行：
  python -m unittest tests.test_search -v
  python tests/test_search.py -v

覆盖契约 4.12：
- >=3 字走 FTS5 trigram MATCH + bm25 排序
- 2 字走 para_fts LIKE（真实命中，按 chapter/para 排序，degraded=false）
- 1 字走 chapters LIKE，degraded=true 且不报错
- snippet 先 HTML 转义再包 <b>
- 索引按需增量补齐、force 重建、limit 上限
"""
from __future__ import annotations

import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from server.app.pipeline import search_index  # noqa: E402

SCHEMA_PATH = ROOT / "server" / "app" / "schema.sql"

_MINIMAL_DDL = """
CREATE TABLE IF NOT EXISTS books (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, author TEXT, format TEXT NOT NULL DEFAULT 'txt',
  file_path TEXT NOT NULL, sha256 TEXT NOT NULL UNIQUE, encoding TEXT,
  total_chars INTEGER NOT NULL DEFAULT 0, chapter_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS chapters (
  id TEXT PRIMARY KEY, book_id TEXT NOT NULL, idx INTEGER NOT NULL, title TEXT NOT NULL,
  chars INTEGER NOT NULL, paras_json TEXT NOT NULL, UNIQUE(book_id, idx)
);
"""


def _create_db(path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(path)
    conn.row_factory = sqlite3.Row
    if SCHEMA_PATH.is_file():
        conn.executescript(SCHEMA_PATH.read_text(encoding="utf-8"))
    else:  # 兼容 A 的 schema.sql 尚未落地的情形
        conn.executescript(_MINIMAL_DDL)
    conn.commit()
    search_index.ensure_schema(conn)
    return conn


def _insert_chapter(conn, book_id: str, idx: int, paragraphs: list[str], title: str | None = None):
    conn.execute(
        "INSERT OR REPLACE INTO chapters(id, book_id, idx, title, chars, paras_json) VALUES (?,?,?,?,?,?)",
        (f"{book_id}-{idx}", book_id, idx, title or f"第{idx + 1}章", sum(len(p) for p in paragraphs),
         json.dumps(paragraphs, ensure_ascii=False)),
    )
    conn.commit()


def _fts_rows(conn) -> int:
    return conn.execute("SELECT count(*) FROM para_fts").fetchone()[0]


class SearchIndexTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="m0_search_")
        self.conn = _create_db(Path(self._tmp.name) / "test.db")
        self.book = "b_test"
        _insert_chapter(self.conn, self.book, 0, ["……示例……", "示例抬起头，看见示例在笑。", "今天天气不错"])
        _insert_chapter(self.conn, self.book, 1, ["<script>示例</script>", "米米米", "另一段"])
        _insert_chapter(self.conn, self.book, 2, ["示例走在路上。"])
        self.assertEqual(search_index.ensure_book_index(self.conn, self.book)["chapters_indexed"], 3)

    def tearDown(self):
        self.conn.close()
        self._tmp.cleanup()

    # ------------------------------------------------ 索引

    def test_schema_idempotent(self):
        search_index.ensure_schema(self.conn)
        search_index.ensure_schema(self.conn)
        self.assertTrue(search_index.has_fts(self.conn))
        self.assertEqual(self.conn.execute(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='search_index_state'"
        ).fetchone()[0], 1)

    def test_incremental_index(self):
        self.assertEqual(search_index.get_state(self.conn, self.book)["chapters_indexed"], 3)
        _insert_chapter(self.conn, self.book, 3, ["新增章节里也有示例。"])
        state = search_index.ensure_book_index(self.conn, self.book)
        self.assertEqual(state["chapters_indexed"], 4)
        # 只补新增章，不重复插入旧章
        self.assertEqual(_fts_rows(self.conn), 7 + 1)
        result = search_index.search(self.conn, self.book, "示例")
        self.assertIn(3, [hit["chapter"] for hit in result["hits"]])

    def test_force_rebuild_no_duplicates(self):
        before = _fts_rows(self.conn)
        search_index.ensure_book_index(self.conn, self.book, force=True)
        self.assertEqual(_fts_rows(self.conn), before)
        self.assertEqual(search_index.get_state(self.conn, self.book)["chapters_indexed"], 3)

    def test_empty_book(self):
        state = search_index.ensure_book_index(self.conn, "b_none")
        self.assertEqual(state["chapters_indexed"], 0)
        result = search_index.search(self.conn, "b_none", "示例")
        self.assertEqual(result["hits"], [])
        self.assertEqual(result["index"]["chapters_indexed"], 0)

    # ------------------------------------------------ 三种查询路径

    def test_two_char_like_search(self):
        result = search_index.search(self.conn, self.book, "示例")
        self.assertFalse(result["index"]["degraded"])
        self.assertEqual(len(result["hits"]), 4)  # ch0p0/ch0p1/ch1p0/ch2p0
        self.assertEqual([(h["chapter"], h["para"]) for h in result["hits"]],
                         [(0, 0), (0, 1), (1, 0), (2, 0)])
        for hit in result["hits"]:
            self.assertIn("<b>示例</b>", hit["snippet"])
            self.assertIn("示例", hit["text"])

    def test_three_char_bm25_search(self):
        result = search_index.search(self.conn, self.book, "示例抬")
        self.assertEqual(len(result["hits"]), 1)
        hit = result["hits"][0]
        self.assertEqual((hit["chapter"], hit["para"]), (0, 1))
        self.assertLess(hit["score"], 0.0)  # bm25 负值，越小越相关
        self.assertIn("<b>示例抬</b>", hit["snippet"])

    def test_three_char_bm25_ordering(self):
        # 命中次数多的段落 bm25 更小（更相关），排前面（合成中性文本）
        _insert_chapter(self.conn, self.book, 4, ["示例文示例文示例文示例文", "示例文"])
        search_index.ensure_book_index(self.conn, self.book)
        hits = search_index.search(self.conn, self.book, "示例文")["hits"]
        self.assertGreaterEqual(len(hits), 2)
        chapters_of_hits = [(h["chapter"], h["para"]) for h in hits]
        self.assertLess(chapters_of_hits.index((4, 0)), chapters_of_hits.index((4, 1)))

    def test_single_char_like_fallback(self):
        result = search_index.search(self.conn, self.book, "米")
        self.assertTrue(result["index"]["degraded"])
        self.assertGreaterEqual(len(result["hits"]), 1)
        self.assertEqual((result["hits"][0]["chapter"], result["hits"][0]["para"]), (1, 1))
        # 单字查询不持久化 degraded（后续正常查询仍是非降级）
        result2 = search_index.search(self.conn, self.book, "示例")
        self.assertFalse(result2["index"]["degraded"])

    def test_single_char_no_match(self):
        result = search_index.search(self.conn, self.book, "龘")
        self.assertEqual(result["hits"], [])
        self.assertTrue(result["index"]["degraded"])

    def test_empty_query_raises(self):
        for bad in ("", "   ", None):
            with self.assertRaises(ValueError):
                search_index.search(self.conn, self.book, bad)

    def test_limit(self):
        self.assertEqual(len(search_index.search(self.conn, self.book, "示例", limit=1)["hits"]), 1)
        self.assertLessEqual(len(search_index.search(self.conn, self.book, "示例", limit=999)["hits"]), 100)
        self.assertEqual(len(search_index.search(self.conn, self.book, "示例", limit=0)["hits"]), 1)


class SnippetTest(unittest.TestCase):
    def test_bold_and_ellipsis(self):
        text = "前文很长" * 20 + "示例" + "后文也很长" * 20
        snippet = search_index.make_snippet(text, "示例", radius=5)
        self.assertIn("<b>示例</b>", snippet)
        self.assertTrue(snippet.startswith("……"))
        self.assertTrue(snippet.endswith("……"))

    def test_html_escape(self):
        snippet = search_index.make_snippet("<script>示例</script>", "示例")
        self.assertNotIn("<script>", snippet)
        self.assertIn("&lt;script&gt;", snippet)
        self.assertIn("<b>示例</b>", snippet)

    def test_multiple_occurrences(self):
        snippet = search_index.make_snippet("示例和示例", "示例")
        self.assertEqual(snippet.count("<b>示例</b>"), 2)

    def test_query_with_html_chars(self):
        snippet = search_index.make_snippet("a<b>示例</b>", "示例")
        self.assertIn("<b>示例</b>", snippet)
        self.assertIn("&lt;b&gt;", snippet)

    def test_no_match_and_empty(self):
        self.assertEqual(search_index.make_snippet("", "示例"), "")
        self.assertNotIn("<b>", search_index.make_snippet("完全无关", "示例"))

    def test_escape_like_wildcards(self):
        self.assertEqual(search_index._escape_like("100%"), "100\\%")
        self.assertEqual(search_index._escape_like("a_b"), "a\\_b")


if __name__ == "__main__":
    unittest.main(verbosity=2)
