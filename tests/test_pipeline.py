"""分段管线单元测试（unittest，无第三方依赖）。

运行：
  python -m unittest tests.test_pipeline -v
  python tests/test_pipeline.py -v

覆盖：
1. 七个特征公式与 spike 参考实现逐项一致（含 tanh 压缩 / 字符集 / 关键词表）
2. 加权打分权重与百分位（bisect，禁止 sigmoid）
3. 段落窗口（target/overlap/min）、按章配额、id 规则、params_hash 稳定性
4. 真书标定：用「干净剥离书头」的章节切分跑全量管线，断言
   docs/spike_report.json 的核心数字（picked_chars 按契约 4.10 允许 ±0.1% 偏差）
"""
from __future__ import annotations

import importlib.util
import json
import re
import statistics
import sys
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from server.app.pipeline.features import (  # noqa: E402
    ACTION_VERBS, FEATURE_NAMES, MONOLOGUE_MARKERS, SCENE_MARKERS, VISUAL_NOUNS,
    extract_features,
)
from server.app.pipeline.scoring import (  # noqa: E402
    WEIGHTS, assign_scores, percentile_rank, raw_score,
)
from server.app.pipeline.segmenter import (  # noqa: E402
    DEFAULT_PARAMS, build_segments, canonical_params, compute_params_hash,
    make_segment_id, normalize_chapters, normalize_params, segment_paragraphs,
    summarize_segments,
)

BOOK_DIR = ROOT / "book"
SPIKE_PATH = ROOT / "prototype" / "spike_segment.py"

# 与 prototype/spike_segment.py 一致（复制常量，避免测试依赖脚本 main）
CHAP_RE = re.compile(r"^\s*(第\s*[0-9零一二三四五六七八九十百千]+\s*[章节回卷]\b.*|Chapter\s+\d+.*|楔子|序章|后记|尾声)\s*$")
_TAG_RE = re.compile(r"^【[^】]*】$")
_SEP_RE = re.compile(r"^[-=*—_]{3,}$")


def _find_book() -> Path | None:
    if not BOOK_DIR.is_dir():
        return None
    books = sorted(BOOK_DIR.glob("*.txt"))
    return books[0] if books else None


def _decode(raw: bytes) -> str:
    for enc in ("utf-8-sig", "utf-8", "gb18030", "utf-16"):
        try:
            return raw.decode(enc)
        except UnicodeDecodeError:
            continue
    raise AssertionError("无法识别编码")


def load_reference_chapters(path: Path) -> list[dict]:
    """按 M0 干净口径切章：丢弃首个标题行前的全部书头/简介，逐条剥章首标签行。

    注意：这是**测试夹具**，只用于标定；生产链路从 chapters 表读 A 的导入结果。
    """
    text = _decode(path.read_bytes())
    lines = [ln.strip() for ln in text.splitlines()]
    starts = [i for i, ln in enumerate(lines) if CHAP_RE.match(ln) and len(ln) <= 40]
    chapters = []
    for k, start in enumerate(starts):
        end = starts[k + 1] if k + 1 < len(starts) else len(lines)
        body = [ln for ln in lines[start + 1:end] if ln]
        while body and (_TAG_RE.match(body[0]) or _SEP_RE.match(body[0])):
            body.pop(0)
        chapters.append({"chapter": k, "title": lines[start], "paragraphs": body})
    return chapters


def _load_spike():
    if not SPIKE_PATH.is_file():
        return None
    spec = importlib.util.spec_from_file_location("spike_segment_reference", SPIKE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FeatureTest(unittest.TestCase):
    def test_feature_names_and_weights(self):
        self.assertEqual(set(FEATURE_NAMES), set(WEIGHTS))
        self.assertEqual(WEIGHTS["scene"], 1.1)  # 场景切换权重最高
        self.assertEqual(WEIGHTS["monologue"], -1.2)  # 心理独白负向
        self.assertAlmostEqual(sum(w for w in WEIGHTS.values() if w > 0), 4.8)
        # 词典非空且与 spike 同为 set（防止有人把列表改成 set 顺序依赖）
        self.assertIsInstance(ACTION_VERBS, set)
        self.assertIsInstance(VISUAL_NOUNS, set)
        self.assertGreater(len(ACTION_VERBS), 40)
        self.assertGreater(len(VISUAL_NOUNS), 40)
        self.assertGreaterEqual(len(SCENE_MARKERS), 20)
        self.assertGreaterEqual(len(MONOLOGUE_MARKERS), 10)

    def test_dialogue_formula(self):
        # 他说，“你好”。 -> 全文 8 字，引号内 4 字 -> 4/8*1.6 = 0.8；该段不以引号开头
        feats = extract_features(["他说，“你好”。"])
        self.assertEqual(feats["dialogue"], 0.8)
        # 全对话 -> 封顶 1.0
        self.assertEqual(extract_features(["“你好”"])["dialogue"], 1.0)
        # 无引号 -> 0
        self.assertEqual(extract_features(["今天天气不错。"])["dialogue"], 0.0)

    def test_tanh_compression(self):
        # 1 个动词 / 61 字 -> tanh(90/61)=0.9；纯动词 -> tanh(90)=1.0（压不到 1 以上）
        short = extract_features(["跑" + "字" * 60])
        dense = extract_features(["跑" * 100])
        self.assertLess(short["action"], dense["action"])
        self.assertGreater(short["action"], 0.85)
        self.assertLessEqual(dense["action"], 1.0)
        self.assertGreater(dense["action"], 0.99)

    def test_scene_and_monologue(self):
        scene = extract_features(["忽然抬头，远处有火光，第二天清晨。"])
        self.assertGreater(scene["scene"], 0.5)
        self.assertEqual(extract_features(["今天天气不错。"])["scene"], 0.0)
        mono = extract_features(["所以，因为，但是，不过，毕竟。"])
        self.assertGreater(mono["monologue"], 0.5)

    def test_rhythm_and_length_fit(self):
        self.assertEqual(extract_features(["短句。"])["rhythm"], 1.0)  # 3 字/段 -> (55-3)/40 > 1 封顶
        self.assertEqual(extract_features(["长" * 60])["rhythm"], 0.0)  # 平均段长 60 >= 55
        at_900 = extract_features(["字" * 900])
        self.assertEqual(at_900["length_fit"], 1.0)
        self.assertLess(extract_features(["字" * 100])["length_fit"], 0.2)
        # chars/paras 统计
        self.assertEqual(at_900["chars"], 900)
        self.assertEqual(at_900["paras"], 1)

    def test_rounding_matches_spike(self):
        spike = _load_spike()
        if spike is None:
            self.skipTest("prototype/spike_segment.py 不在")
        # 脱敏：用中性合成文本覆盖相同特征维度（对话/动作/拟声/场景），仍与 spike 逐项对比
        chunks = [
            ["“示例对话还有很多……”示例角色面露难色，目前没有任何余量可以腾出来。"],
            ["示例角色刚有动作，凌厉的目光随即扫了过来。"],
            ["哗啦啦！哗啦啦！"],
            ["忽然，远处传来一声爆响，他冲过去抓起剑，抬头看见火光。" * 3],
        ]
        for chunk in chunks:
            expected = spike.features(chunk)
            actual = extract_features(chunk)
            for key in FEATURE_NAMES + ("chars", "paras"):
                self.assertEqual(actual[key], expected[key], f"{key} 与 spike 不一致")
            self.assertEqual(raw_score(actual), expected["raw"])


class ScoringTest(unittest.TestCase):
    def test_raw_score_weights(self):
        zeros = {name: 0.0 for name in FEATURE_NAMES}
        self.assertEqual(raw_score(zeros), 0.0)
        only_scene = dict(zeros, scene=1.0)
        self.assertEqual(raw_score(only_scene), 1.1)
        only_mono = dict(zeros, monologue=1.0)
        self.assertEqual(raw_score(only_mono), -1.2)
        mixed = dict(zeros, dialogue=0.5, action=0.5, visual=0.5, scene=0.5,
                     rhythm=0.5, length_fit=0.5, monologue=0.5)
        expected = round(0.9 * 0.5 + 0.9 * 0.5 + 0.7 * 0.5 + 1.1 * 0.5 + 0.7 * 0.5 + 0.5 * 0.5 - 1.2 * 0.5, 3)
        self.assertEqual(raw_score(mixed), expected)

    def test_percentile_rank(self):
        self.assertEqual(percentile_rank([]), [])
        self.assertEqual(percentile_rank([1, 2, 3, 4]), [0.125, 0.375, 0.625, 0.875])
        # 并列值相同百分位（bisect_right：两个 5 都排到 2/2）
        self.assertEqual(percentile_rank([5, 5]), [0.75, 0.75])
        # 范围 0..1，最大值 < 1
        ranks = percentile_rank(list(range(100)))
        self.assertTrue(all(0.0 <= r <= 1.0 for r in ranks))
        self.assertLess(max(ranks), 1.0)
        self.assertEqual(ranks[-1], 0.995)

    def test_assign_scores_inplace(self):
        segments = [{"raw": 1.0}, {"raw": 3.0}, {"raw": 2.0}]
        scores = assign_scores(segments)
        self.assertEqual(scores, [0.167, 0.833, 0.5])
        self.assertEqual([s["score"] for s in segments], scores)


class SegmenterTest(unittest.TestCase):
    def test_window_target_and_overlap(self):
        paras = ["a" * 450, "b" * 450, "c" * 450, "d" * 450]
        windows = segment_paragraphs(paras, DEFAULT_PARAMS)
        self.assertEqual(
            windows,
            [
                {"start_para": 0, "end_para": 1},
                {"start_para": 1, "end_para": 2},
                {"start_para": 2, "end_para": 3},
                {"start_para": 3, "end_para": 3},
            ],
        )

    def test_min_chars_and_short_tail(self):
        # 单段短章仍然保留（否则该章无候选）
        self.assertEqual(segment_paragraphs(["x" * 100], DEFAULT_PARAMS), [{"start_para": 0, "end_para": 0}])
        # overlap=0 时，章末不足 min_chars 的尾巴丢弃
        windows = segment_paragraphs(["x" * 900, "y" * 100], {"overlap_paras": 0})
        self.assertEqual(windows, [{"start_para": 0, "end_para": 0}])

    def test_validate_params(self):
        self.assertEqual(normalize_params(None), DEFAULT_PARAMS)
        self.assertEqual(normalize_params({"target": 800, "min": 300})["target_chars"], 800)
        self.assertEqual(normalize_params({"min": 300})["min_chars"], 300)
        for bad in ({"target_chars": "900"}, {"target_chars": True}, {"bogus": 1},
                    {"target_chars": 1.5}, {"overlap_paras": -1}):
            with self.assertRaises(ValueError):
                normalize_params(bad)

    def test_params_hash_stable(self):
        params = {"target_chars": 900, "overlap_paras": 1, "min_chars": 400, "max_chars": 1500}
        reordered = {"max_chars": 1500, "min_chars": 400, "overlap_paras": 1, "target_chars": 900}
        h1 = compute_params_hash("paragraph_window", normalize_params(params), 1)
        h2 = compute_params_hash("paragraph_window", normalize_params(reordered), 1)
        self.assertEqual(h1, h2)
        self.assertEqual(len(h1), 16)
        int(h1, 16)  # 是十六进制
        self.assertNotEqual(h1, compute_params_hash("paragraph_window", normalize_params(params), 2))
        self.assertNotEqual(h1, compute_params_hash("paragraph_window", normalize_params({"target_chars": 800}), 1))
        self.assertEqual(canonical_params("paragraph_window", normalize_params(params), 1),
                         canonical_params("paragraph_window", normalize_params(reordered), 1))

    def test_normalize_chapters(self):
        chapters = [
            {"idx": 0, "title": "第1章", "paras": ["a", "", "  ", "b"]},
            {"paras": ["c"]},
        ]
        norm = normalize_chapters(chapters)
        self.assertEqual(norm[0]["paragraphs"], ["a", "b"])  # 空段剔除
        self.assertEqual(norm[0]["chapter"], 0)
        self.assertEqual(norm[1]["chapter"], 1)  # 缺 chapter 用位置兜底
        self.assertEqual(norm[1]["title"], "第2章")

    def test_build_segments_ids_selection_stats(self):
        chapters = [
            {"chapter": 0, "title": "第1章", "paragraphs": ["跑" * 500, "看" * 500]},
            {"chapter": 3, "title": "第4章", "paragraphs": ["忽" * 500, "所" * 500]},
        ]
        result = build_segments(chapters, per_chapter=1)
        self.assertEqual(result["strategy"], "paragraph_window")
        self.assertEqual(result["params"], DEFAULT_PARAMS)
        segs = result["segments"]
        self.assertEqual(len(segs), 4)
        self.assertEqual([s["id"] for s in segs], ["c1-s0", "c1-s1", "c4-s0", "c4-s1"])
        # 每章恰好选 1 个，且是该章分数最高的
        for chapter in (0, 3):
            group = [s for s in segs if s["chapter"] == chapter]
            self.assertEqual(sum(s["selected"] for s in group), 1)
            picked = [s for s in group if s["selected"] == 1][0]
            self.assertEqual(picked["score"], max(s["score"] for s in group))
        self.assertEqual(result["stats"]["selected"], 2)
        self.assertEqual(result["stats"]["segments"], 4)
        self.assertEqual(result["stats"]["picked_chars"], sum(s["chars"] for s in segs if s["selected"]))
        self.assertEqual(result["stats"]["pass_rate"], 0.5)
        self.assertEqual(result["stats"]["est_llm_input_tokens"],
                         int(result["stats"]["picked_chars"] * 0.75))

    def test_per_chapter_quota_and_determinism(self):
        chapters = [{"chapter": c, "title": f"第{c + 1}章",
                     "paragraphs": ["跑" * 900, "看" * 900, "想" * 900]} for c in range(3)]
        one = build_segments(chapters, per_chapter=1)
        two = build_segments(chapters, per_chapter=2)
        self.assertEqual(one["stats"]["selected"], 3)
        self.assertEqual(two["stats"]["selected"], 6)
        again = build_segments(chapters, per_chapter=2)
        self.assertEqual(json.dumps(two["segments"], ensure_ascii=False), json.dumps(again["segments"], ensure_ascii=False))

    def test_summarize_empty(self):
        stats = summarize_segments([])
        self.assertEqual(stats["segments"], 0)
        self.assertEqual(stats["selected"], 0)


class CalibrationTest(unittest.TestCase):
    """真书标定（M0 验收核心）。书不在时跳过，不依赖固定绝对路径。"""

    @classmethod
    def setUpClass(cls):
        cls.book = _find_book()
        if cls.book is None:
            raise unittest.SkipTest("book/*.txt 不存在，跳过真书标定")
        cls.chapters = load_reference_chapters(cls.book)

    def test_chapter_split_clean_baseline(self):
        self.assertEqual(len(self.chapters), 234)
        total_paras = sum(len(ch["paragraphs"]) for ch in self.chapters)
        self.assertEqual(total_paras, 15245, "干净口径段落总数应为 15245")
        # 书头/简介不得污染第 1 章
        first = "\n".join(self.chapters[0]["paragraphs"])
        self.assertNotIn("书籍信息", first)
        self.assertNotIn("【简介】", first)
        # 结构性断言（脱敏）：首段非空、非书头元信息；不硬编码原文
        p0 = self.chapters[0]["paragraphs"][0]
        p32 = self.chapters[32]["paragraphs"][0]
        self.assertIsInstance(p0, str)
        self.assertGreater(len(p0), 0)
        self.assertNotIn("书籍信息", p0)
        self.assertIsInstance(p32, str)
        self.assertGreater(len(p32), 0)

    def test_calibration_numbers(self):
        started = time.perf_counter()
        result = build_segments(self.chapters, params=None, per_chapter=1)
        elapsed = time.perf_counter() - started
        stats = result["stats"]
        self.assertEqual(stats["segments"], 475)
        self.assertEqual(stats["avg_segment_chars"], 913)
        self.assertEqual(stats["selected"], 234)
        self.assertEqual(stats["raw_min"], 0.505)
        self.assertEqual(stats["raw_p50"], 1.937)
        self.assertEqual(stats["raw_p90"], 2.63)
        self.assertEqual(stats["pass_rate"], 0.493)
        self.assertEqual(stats["picked_char_ratio"], 0.494)
        # 契约 4.10：允许因书头/简介剥离产生 <=0.1% 偏差（214184 -> 214167）
        self.assertLessEqual(abs(stats["picked_chars"] - 214184), 500,
                             f"picked_chars={stats['picked_chars']} 超出 214184±500")
        self.assertLessEqual(abs(stats["est_llm_input_tokens"] - 160638), 500)
        # 全量纯函数分段耗时应远小于 3s 验收线
        self.assertLess(elapsed, 3.0)
        top = sorted(result["segments"], key=lambda s: -s["score"])[0]
        self.assertEqual(top["chapter"], 32)
        # 结构性断言（脱敏）：head 非空，且是第 33 章首段的前缀（不硬编码原文）
        head = top["head"]
        self.assertIsInstance(head, str)
        self.assertGreater(len(head), 0)
        self.assertTrue(self.chapters[32]["paragraphs"][0].startswith(head[:min(20, len(head))]))

    def test_segment_windows_match_spike(self):
        spike = _load_spike()
        if spike is None:
            self.skipTest("prototype/spike_segment.py 不在")
        for chapter in self.chapters[:30]:
            expected = spike.segment_chapter(chapter["paragraphs"])
            windows = segment_paragraphs(chapter["paragraphs"], DEFAULT_PARAMS)
            actual = [chapter["paragraphs"][w["start_para"]: w["end_para"] + 1] for w in windows]
            self.assertEqual(actual, expected, f"第 {chapter['chapter'] + 1} 章窗口与 spike 不一致")


if __name__ == "__main__":
    unittest.main(verbosity=2)
