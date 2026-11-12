"""分段特征提取（纯函数：不碰数据库、不碰 FastAPI）。

来源：prototype/spike_segment.py（已在真书 docs/spike_report.json 标定）。
字符集、关键词表、tanh 压缩系数与统计口径**必须逐字一致**，否则标定数字会漂移
（见 docs/ARCHITECTURE.md 6.3「特征与打分（已用真书标定）」）。

七个特征（均为 0..1）：
- dialogue   引号内字符占比 / 对话起始段占比
- action     动作动词密度（tanh 压缩）
- visual     可视名词密度（tanh 压缩）
- scene      场景切换标记密度（tanh 压缩，权重最高）
- rhythm     平均段长越短越高（动作戏段落碎）
- length_fit 对 900 字窗口的高斯贴合（防超长）
- monologue  心理连接词密度（tanh 压缩，负向权重）
"""
from __future__ import annotations

import math
import re
import statistics
from typing import Iterable, Sequence

# --- 特征词典（与 prototype/spike_segment.py 完全一致，不要随手增删） ---
ACTION_VERBS = set("踢 扛 跑 抓 抱 挥 砸 冲 退 扑 躲 拉 推 拽 拧 踹 翻 跳 撞 摔 掐 捏 撩 甩 撑 撞开 掀 撕 咬 拔 插 挑 挡 架 扯 拖 抬 背 扶 摁 按 摸 拍 敲 踹开".split())
VISUAL_NOUNS = set("脸 眼 眉 发 唇 指 肩 腰 腿 脚 血 汗 泪 天空 云 灯 门 窗 桌 椅 墙 走廊 教室 食堂 操场 街道 雨 雪 雾 火光 影子 飞行器 制服 裙 帽 鞋 手套 项链 镜子 塔 楼梯 阳台 书 书包 手机 武器 剑 刀".split())
SCENE_MARKERS = [
    "几分钟后", "片刻后", "第二天", "翌日", "这时", "此时", "忽然", "突然", "与此同时",
    "夜幕", "黄昏", "清晨", "入夜", "远处", "身后", "门外", "窗外", "抬头", "低头",
    "转头", "回过头", "不知过了多久", "当他", "当她",
]
MONOLOGUE_MARKERS = [
    "所以", "因为", "但是", "不过", "毕竟", "也就是说", "换句话说", "总之", "换言之",
    "思考", "回想", "意识到", "明白",
]

# 窗口目标字数（length_fit 的高斯中心，与 spike 一致）
TARGET_CHARS = 900

FEATURE_NAMES = ("dialogue", "action", "visual", "scene", "rhythm", "length_fit", "monologue")

_QUOTED_RE = re.compile(r"“[^”]*”")


def dialogue_score(chunk: Sequence[str]) -> float:
    """引号内字符占比与对话起始段占比的较大者（封顶 1）。"""
    txt = "".join(chunk)
    n = max(len(txt), 1)
    quoted = sum(len(m) for m in _QUOTED_RE.findall(txt))
    dlg_lines = sum(1 for p in chunk if p.startswith("“") or p.startswith("「"))
    return min(1.0, max(quoted / n * 1.6, dlg_lines / max(len(chunk), 1) * 1.2))


def action_score(txt: str) -> float:
    """动作动词密度，tanh 压缩避免长片段天然偏高。"""
    n = max(len(txt), 1)
    return math.tanh(sum(txt.count(v) for v in ACTION_VERBS) / n * 90)


def visual_score(txt: str) -> float:
    n = max(len(txt), 1)
    return math.tanh(sum(txt.count(w) for w in VISUAL_NOUNS) / n * 90)


def scene_score(txt: str) -> float:
    n = max(len(txt), 1)
    return math.tanh(sum(txt.count(m) for m in SCENE_MARKERS) / n * 260)


def monologue_score(txt: str) -> float:
    n = max(len(txt), 1)
    return math.tanh(sum(txt.count(m) for m in MONOLOGUE_MARKERS) / n * 160)


def rhythm_score(chunk: Sequence[str]) -> float:
    """平均段长越短 -> 越接近动作/对话场景（55 字以上归零）。"""
    if not chunk:
        return 0.0
    para_len = statistics.mean(len(p) for p in chunk)
    if para_len < 55:
        return min(1.0, max(0.0, (55 - para_len) / 40))
    return 0.0


def length_fit_score(txt: str) -> float:
    """对 900 字窗口的高斯贴合（σ=420）。"""
    return math.exp(-((len(txt) - TARGET_CHARS) ** 2) / (2 * 420 ** 2))


def extract_features(chunk: Iterable[str]) -> dict:
    """返回 7 个特征（各 round 3）+ chars/paras。

    注意：与 spike 一致，raw 分在 scoring.raw_score 里用**四舍五入后的特征**计算。
    """
    paras = [str(p) for p in chunk]
    txt = "".join(paras)
    return {
        "dialogue": round(dialogue_score(paras), 3),
        "action": round(action_score(txt), 3),
        "visual": round(visual_score(txt), 3),
        "scene": round(scene_score(txt), 3),
        "monologue": round(monologue_score(txt), 3),
        "rhythm": round(rhythm_score(paras), 3),
        "length_fit": round(length_fit_score(txt), 3),
        "chars": len(txt),
        "paras": len(paras),
    }
