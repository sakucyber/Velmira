"""加权打分与语料内百分位（纯函数）。

关键决策（docs/ARCHITECTURE.md 6.3）：**禁止 sigmoid / logistic 归一化**。
首版 sigmoid 后 p50=0.97 全部顶到 1，排序失效；改用 bisect 语料内百分位后
阈值不随书漂移。score ∈ [0,1]，公式与 spike 完全一致。
"""
from __future__ import annotations

import bisect
from typing import Sequence

from .features import FEATURE_NAMES

# 权重已用真书标定（顺序即 FEATURE_NAMES），不要单方面改数字
WEIGHTS = {
    "dialogue": 0.9,
    "action": 0.9,
    "visual": 0.7,
    "scene": 1.1,
    "rhythm": 0.7,
    "length_fit": 0.5,
    "monologue": -1.2,
}


def raw_score(features: dict) -> float:
    """原始加权分（用 round 后的特征算，round 3，与 spike 一致）。"""
    total = sum(WEIGHTS[name] * float(features.get(name, 0.0) or 0.0) for name in FEATURE_NAMES)
    return round(total, 3)


def percentile_rank(values: Sequence[float]) -> list[float]:
    """把原始分映射到 [0,1] 的语料内百分位。

    与 spike 一致：rank = (bisect_right(sorted, v) - 0.5) / n，即最大值 ≈ 0.999。
    并列值得到相同百分位（bisect_right）。
    """
    n = len(values)
    if n == 0:
        return []
    ordered = sorted(values)
    return [round((bisect.bisect_right(ordered, v) - 0.5) / n, 3) for v in values]


def assign_scores(segments: Sequence[dict]) -> list[float]:
    """就地为 segments 写入 score，返回 score 列表（读取 seg['raw']）。"""
    scores = percentile_rank([float(seg["raw"]) for seg in segments])
    for seg, score in zip(segments, scores):
        seg["score"] = score
    return scores
