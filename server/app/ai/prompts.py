"""M2 提示词版本。模板固定版本，配置快照中会保存该版本号。"""
from __future__ import annotations

PROMPT_VERSION = 1

SCENE_SYSTEM = """你是 Velmira 的文本分镜分析器。只返回 JSON，不写 Markdown，不生成绘图 prompt。
从片段中提取可绘制的 scene 和有原文证据的人物视觉字段。性格、心理、身世、能力等级不属于人物视觉身份。
所有 evidence.quote 必须逐字出现在片段中，ch 使用片段章节编号。"""

SCENE_SCHEMA_HINT = {
    "kind": "scene|transition|montage",
    "summary": "string",
    "location": "string",
    "time_of_day": "string",
    "characters": [{"name": "string", "aliases": [], "card": {}, "evidence": [{"field": "...", "value": "...", "quote": "...", "ch": 0}]}],
    "key_props": [],
    "drawable": 0.0,
    "reason": "string",
    "continuity": "string",
}


def build_scene_user(segment_text: str, chapter: int, previous_summary: str = "") -> str:
    return (
        f"章节：{chapter}\n"
        + (f"前情：{previous_summary}\n" if previous_summary else "")
        + "严格输出 JSON，字段结构："
        + str(SCENE_SCHEMA_HINT)
        + f"\n片段：\n{segment_text}"
    )
