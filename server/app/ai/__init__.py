"""M2 文本抽取核心：provider、结构化校验、人物卡合并。"""

from .models import (
    CARD_FIELDS,
    normalize_character,
    normalize_scene,
    render_identity,
    identity_hash,
)

__all__ = [
    "CARD_FIELDS",
    "normalize_character",
    "normalize_scene",
    "render_identity",
    "identity_hash",
]
