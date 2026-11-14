"""Grouped configuration metadata for the settings UI.

The individual domain APIs remain backwards compatible (reading, AI, manga,
translation, search and system/security).  This small read-only endpoint gives
clients one stable way to build a grouped settings navigator without copying
the server's information architecture into the frontend.
"""
from fastapi import APIRouter

from ..ai import routing
from ..manga.models import GENERATION_PRESETS

router = APIRouter()


DOMAINS = [
    {"id": "reader", "label": "阅读", "sections": ["reading", "offline"],
     "description": "书库、阅读器、快捷键与离线下载。"},
    {"id": "ai", "label": "AI 引擎", "sections": ["models", "memory", "prompts"],
     "description": "文本/图像模型、长期记忆、提示词与模型策略。"},
    {"id": "extensions", "label": "扩展功能", "sections": ["manga", "translation", "search"],
     "description": "自动漫画、翻译和联网搜索。"},
    {"id": "system", "label": "系统与网络", "sections": ["security", "sync"],
     "description": "局域网访问、配对、同步和设备管理。"},
]


def _manga_presets():
    return [{"id": key, "label": value["label"], "description": value["description"],
             "max_attempts": value["max_attempts"], "timeout": value["timeout"],
             "face_threshold": value["face_threshold"],
             "review_storyboards": value["review_storyboards"]}
            for key, value in GENERATION_PRESETS.items()]


@router.get("/api/config/domains")
def get_config_domains():
    """Return the canonical settings groups and their section identifiers."""
    return {"domains": DOMAINS}


@router.get("/api/config/presets")
def get_config_presets():
    """Return high-level policies without any credentials or user settings."""
    return {"text": routing.public_presets("text"),
            "image": routing.public_presets("image"),
            "manga": _manga_presets()}
