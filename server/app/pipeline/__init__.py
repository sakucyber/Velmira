"""M0 管线：分段特征 / 打分 / 段落窗口 / FTS 检索索引。

本包**纯函数 + sqlite3**，不依赖 FastAPI；HTTP 与 DB 连接分别由
server/app/api/segments.py、server/app/api/search.py 负责。
"""
from .features import FEATURE_NAMES, extract_features
from .scoring import WEIGHTS, percentile_rank, raw_score
from .segmenter import (
    DEFAULT_PARAMS,
    STRATEGY_PARAGRAPH_WINDOW,
    SUPPORTED_STRATEGIES,
    build_segments,
    compute_params_hash,
    normalize_params,
    segment_paragraphs,
    summarize_segments,
)

__all__ = [
    "FEATURE_NAMES",
    "extract_features",
    "WEIGHTS",
    "percentile_rank",
    "raw_score",
    "DEFAULT_PARAMS",
    "STRATEGY_PARAGRAPH_WINDOW",
    "SUPPORTED_STRATEGIES",
    "build_segments",
    "compute_params_hash",
    "normalize_params",
    "segment_paragraphs",
    "summarize_segments",
]
