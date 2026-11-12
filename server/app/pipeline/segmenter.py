"""段落窗口分段 + 按章配额选片（纯函数：不碰 DB / FastAPI）。

输入（与 docs/API-CONTRACT.md 无关，是 pipeline 内部结构）::

    [{"chapter": 0, "title": "第1章 示例章节", "paragraphs": ["...", "..."]}]

输出::

    {"strategy", "params", "params_hash", "per_chapter", "segments", "stats"}

segments 每项：id / chapter / idx / strategy / start_para / end_para / chars / text /
head / features / raw / score / selected / title。

策略：paragraph_window（默认，已标定）。其余策略（chapter / dialogue_turn / fixed /
semantic / manual）M0 只占位，不实现，调用会抛 ValueError。

段落窗口算法与 prototype/spike_segment.py 完全一致：
贪心按段累加，size >= target 时收敛一个窗口，窗口间重叠 overlap 段；
章末剩余不足 min_chars 的尾巴丢弃（除非该章一个窗口都没有）。
max_chars 只做参数记录：段不可再分，窗口天然可越过上限，强切会破坏语义并让标定漂移。
"""
from __future__ import annotations

import hashlib
import json
import statistics
from typing import Iterable, Sequence

from .features import extract_features
from .scoring import assign_scores, raw_score

STRATEGY_PARAGRAPH_WINDOW = "paragraph_window"
SUPPORTED_STRATEGIES = (STRATEGY_PARAGRAPH_WINDOW,)

# 与 docs/API-CONTRACT.md 4.10 请求体默认值一致
DEFAULT_PARAMS = {
    "target_chars": 900,
    "overlap_paras": 1,
    "min_chars": 400,
    "max_chars": 1500,
}

# 兼容简写（前端/脚本可能用 target/overlap/min/max）
_PARAM_ALIASES = {
    "target": "target_chars",
    "overlap": "overlap_paras",
    "min": "min_chars",
    "max": "max_chars",
}


def _as_int(name: str, value) -> int:
    """只接受整数值（bool/浮点小数/字符串一律拒绝）。"""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"参数 {name} 必须是整数")
    iv = int(value)
    if float(value) != iv:
        raise ValueError(f"参数 {name} 必须是整数")
    return iv


def normalize_params(params: dict | None = None) -> dict:
    """补默认值/别名并校验，返回新 dict（键固定为 DEFAULT_PARAMS 的四个键）。"""
    raw = dict(params or {})
    merged = dict(DEFAULT_PARAMS)
    for key, value in raw.items():
        canon = _PARAM_ALIASES.get(key, key)
        if canon not in DEFAULT_PARAMS:
            raise ValueError(f"未知参数: {key}")
        merged[canon] = _as_int(canon, value)
    if merged["target_chars"] < 1:
        raise ValueError("target_chars 必须 >= 1")
    if merged["overlap_paras"] < 0:
        raise ValueError("overlap_paras 必须 >= 0")
    if merged["min_chars"] < 0:
        raise ValueError("min_chars 必须 >= 0")
    if merged["max_chars"] < 1:
        raise ValueError("max_chars 必须 >= 1")
    return merged


def canonical_params(strategy: str, params: dict, per_chapter: int = 1) -> str:
    """规范化 JSON：键排序 + 紧凑分隔符，保证同参数得到同 hash。"""
    payload = {
        "strategy": str(strategy),
        "params": {key: int(params[key]) for key in sorted(DEFAULT_PARAMS)},
        "per_chapter": int(per_chapter),
    }
    return json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def compute_params_hash(strategy: str, params: dict, per_chapter: int = 1) -> str:
    """params_hash = sha256(canonical json)[:16]（与契约 4.10 一致）。

    per_chapter 也参与 hash：selected 标记由配额决定，若不参与会导致
    「换配额但命中缓存」返回错误统计。
    """
    return hashlib.sha256(canonical_params(strategy, params, per_chapter).encode("utf-8")).hexdigest()[:16]


def normalize_chapters(chapters: Iterable[dict]) -> list[dict]:
    """归一化章节输入：兼容 chapter/idx、paragraphs/paras 两种键名。"""
    out: list[dict] = []
    for pos, ch in enumerate(chapters):
        if not isinstance(ch, dict):
            raise ValueError("chapters 每项必须是 dict")
        idx = ch.get("chapter", ch.get("idx", pos))
        idx = int(idx)
        title = str(ch.get("title") or f"第{idx + 1}章")
        paras = ch.get("paragraphs", ch.get("paras"))
        if paras is None:
            paras = []
        if isinstance(paras, str):
            paras = paras.splitlines()
        cleaned = [str(p) for p in paras if p is not None and str(p).strip() != ""]
        out.append({"chapter": idx, "title": title, "paragraphs": cleaned})
    return out


def segment_paragraphs(paragraphs: Sequence[str], params: dict | None = None) -> list[dict]:
    """把一个章节的段落切成窗口，返回 [{start_para, end_para}]（闭区间下标）。"""
    p = normalize_params(params)
    target = p["target_chars"]
    overlap = p["overlap_paras"]
    min_chars = p["min_chars"]

    windows: list[dict] = []
    cur: list[int] = []
    size = 0
    for pi, para in enumerate(paragraphs):
        cur.append(pi)
        size += len(para)
        if size >= target:
            windows.append({"start_para": cur[0], "end_para": cur[-1]})
            cur = cur[-overlap:] if overlap else []
            size = sum(len(paragraphs[i]) for i in cur)
    if cur and (size >= min_chars or not windows):
        windows.append({"start_para": cur[0], "end_para": cur[-1]})
    return windows


def make_segment_id(chapter: int, idx: int) -> str:
    """契约 4.11 示例：章节 idx 0-based，展示名 +1 -> c33-s0。"""
    return f"c{int(chapter) + 1}-s{int(idx)}"


def select_top_per_chapter(segments: list[dict], per_chapter: int) -> int:
    """每章按 score 降序取前 per_chapter 个，写 selected=1；返回选中数。"""
    if per_chapter < 0:
        raise ValueError("per_chapter 必须 >= 0")
    if per_chapter == 0:
        return 0
    by_chapter: dict[int, list[dict]] = {}
    for seg in segments:
        by_chapter.setdefault(seg["chapter"], []).append(seg)
    picked = 0
    for group in by_chapter.values():
        for seg in sorted(group, key=lambda s: (-s["score"], s["idx"]))[:per_chapter]:
            seg["selected"] = 1
            picked += 1
    return picked


def summarize_segments(segments: Sequence[dict], source_chars: int | None = None) -> dict:
    """汇总统计，字段口径与 docs/spike_report.json 对齐。

    注意 picked_char_ratio 沿用 spike 口径 = picked_chars / 所有候选中文字数
    （不是全书字数；全书占比另见 picked_char_ratio_book）。
    """
    n = len(segments)
    if n == 0:
        return {
            "segments": 0, "avg_segment_chars": 0, "selected": 0, "picked_chars": 0,
            "picked_char_ratio": 0.0, "est_llm_input_tokens": 0, "pass_rate": 0.0,
            "raw_min": 0.0, "raw_p50": 0.0, "raw_p90": 0.0, "raw_max": 0.0,
            "score_p50": 0.0, "score_p90": 0.0, "score_max": 0.0,
            "total_segment_chars": 0,
        }
    chars = [int(s.get("chars", 0)) for s in segments]
    raws = sorted(float(s["raw"]) for s in segments)
    scores = sorted(float(s.get("score", 0.0)) for s in segments)
    picked = [s for s in segments if int(s.get("selected", 0)) == 1]
    picked_chars = sum(int(s.get("chars", 0)) for s in picked)
    total_seg_chars = sum(chars)
    p90 = int(n * 0.9)
    if p90 >= n:  # pragma: no cover - n>0 时不可达
        p90 = n - 1
    stats = {
        "segments": n,
        "avg_segment_chars": round(statistics.mean(chars)),
        "selected": len(picked),
        "picked_chars": picked_chars,
        "picked_char_ratio": round(picked_chars / total_seg_chars, 3) if total_seg_chars else 0.0,
        "est_llm_input_tokens": int(picked_chars * 0.75),
        "pass_rate": round(len(picked) / n, 3),
        "raw_min": round(raws[0], 3),
        "raw_p50": round(statistics.median(raws), 3),
        "raw_p90": round(raws[p90], 3),
        "raw_max": round(raws[-1], 3),
        "score_p50": round(statistics.median(scores), 3),
        "score_p90": round(scores[p90], 3),
        "score_max": round(scores[-1], 3),
        "total_segment_chars": total_seg_chars,
    }
    if source_chars:
        stats["source_chars"] = int(source_chars)
        stats["picked_char_ratio_book"] = round(picked_chars / int(source_chars), 3)
    return stats


def build_segments(
    chapters: Iterable[dict],
    params: dict | None = None,
    per_chapter: int = 1,
    strategy: str = STRATEGY_PARAGRAPH_WINDOW,
) -> dict:
    """主入口：章节 -> 候选片段 + 分数 + 选片，纯内存计算。"""
    if strategy not in SUPPORTED_STRATEGIES:
        raise ValueError(f"不支持的策略: {strategy}")
    norm_params = normalize_params(params)
    per_chapter = int(per_chapter)
    if per_chapter < 0 or per_chapter > 50:
        raise ValueError("per_chapter 必须在 0..50")
    norm_chapters = normalize_chapters(chapters)

    segments: list[dict] = []
    for chapter in norm_chapters:
        paras = chapter["paragraphs"]
        for idx, window in enumerate(segment_paragraphs(paras, norm_params)):
            chunk = paras[window["start_para"]: window["end_para"] + 1]
            features = extract_features(chunk)
            segments.append({
                "id": make_segment_id(chapter["chapter"], idx),
                "chapter": chapter["chapter"],
                "idx": idx,
                "strategy": strategy,
                "start_para": window["start_para"],
                "end_para": window["end_para"],
                "chars": features["chars"],
                "text": "\n".join(chunk),
                "head": chunk[0][:60] if chunk else "",
                "features": features,
                "raw": raw_score(features),
                "score": 0.0,
                "selected": 0,
                "title": chapter["title"],
            })

    assign_scores(segments)
    selected = select_top_per_chapter(segments, per_chapter)
    source_chars = sum(len(p) for ch in norm_chapters for p in ch["paragraphs"])
    stats = summarize_segments(segments, source_chars=source_chars)
    stats["selected"] = selected
    stats["pass_rate"] = round(selected / len(segments), 3) if segments else 0.0

    return {
        "strategy": strategy,
        "params": norm_params,
        "params_hash": compute_params_hash(strategy, norm_params, per_chapter),
        "per_chapter": per_chapter,
        "segments": segments,
        "stats": stats,
    }
