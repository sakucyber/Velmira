"""分段策略可行性验证（spike）
输入：book/*.txt（UTF-8 / GBK 自动探测，第N章 切分）
输出：docs/spike_report.json + 控制台统计

目的：验证「本地规则预筛 -> LLM 精排」两段式分段在真实网文上的可行性与成本。
仅用标准库。
"""

import json
import math
import os
import re
import statistics
import sys

BOOK_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "book")
OUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs")

CHAP_RE = re.compile(r"^\s*(第\s*[0-9零一二三四五六七八九十百千]+\s*[章节回卷]\b.*|Chapter\s+\d+.*|楔子|序章|后记|尾声)\s*$")
META_HINT = re.compile(r"^(书名|原名|作者|连载状态|字数|章节数|书籍\s*ID|阅读量|最后更新时间|书籍信息|【简介】)[:：]?")

# --- 特征词典（最小可用集，后续可在配置中扩展/由 LLM 冷启动） ---
ACTION_VERBS = set("踢 扛 跑 抓 抱 挥 砸 冲 退 扑 躲 拉 推 拽 拧 踹 翻 跳 撞 摔 掐 捏 撩 甩 撑 撞开 掀 撕 咬 拔 插 挑 挡 架 扯 拖 抬 背 扶 摁 按 摸 拍 敲 踹开".split())
VISUAL_NOUNS = set("脸 眼 眉 发 唇 指 肩 腰 腿 脚 血 汗 泪 天空 云 灯 门 窗 桌 椅 墙 走廊 教室 食堂 操场 街道 雨 雪 雾 火光 影子 飞行器 制服 裙 帽 鞋 手套 项链 镜子 塔 楼梯 阳台 书 书包 手机 武器 剑 刀".split())
SCENE_MARKERS = ["几分钟后", "片刻后", "第二天", "翌日", "这时", "此时", "忽然", "突然", "与此同时", "夜幕", "黄昏", "清晨", "入夜",
                 "远处", "身后", "门外", "窗外", "抬头", "低头", "转头", "回过头", "不知过了多久", "当他", "当她"]
MONOLOGUE_MARKERS = ["所以", "因为", "但是", "不过", "毕竟", "也就是说", "换句话说", "总之", "换言之", "思考", "回想", "意识到", "明白"]

OVERLAP_PARAS = 1        # 窗口重叠段数
MIN_SEG, TARGET_SEG, MAX_SEG = 400, 900, 1500


def load_text(path):
    raw = open(path, "rb").read()
    for enc in ("utf-8-sig", "utf-8", "gb18030", "utf-16"):
        try:
            return raw.decode(enc), enc
        except UnicodeDecodeError:
            continue
    raise SystemExit("无法识别编码")


def split_chapters(text):
    """返回 [{'title','paras':[str]}]，并剔除卷首书籍信息块。"""
    lines = [ln.strip() for ln in text.splitlines()]
    starts = [i for i, ln in enumerate(lines) if CHAP_RE.match(ln) and len(ln) <= 40]
    chapters = []
    for k, i in enumerate(starts):
        end = starts[k + 1] if k + 1 < len(starts) else len(lines)
        body = [ln for ln in lines[:end] if ln and not META_HINT.match(ln)] if k == 0 else \
               [ln for ln in lines[i + 1:end] if ln]
        # 去掉章首的标签行/分隔线（如【类型标签】、***）
        while body and (re.match(r"^【[^】]*】$", body[0]) or re.match(r"^[-=*—]{3,}$", body[0])):
            body.pop(0)
        chapters.append({"title": lines[i], "paras": body})
    return chapters


def segment_chapter(paras, target=TARGET_SEG, overlap=OVERLAP_PARAS):
    """段落贪心聚合到 target 字符，窗口间重叠 overlap 段。"""
    segs, cur, size = [], [], 0
    for p in paras:
        cur.append(p)
        size += len(p)
        if size >= target:
            segs.append(cur)
            cur = cur[-overlap:] if overlap else []
            size = sum(len(x) for x in cur)
    if cur and (size >= MIN_SEG or not segs):
        segs.append(cur)
    return segs


def features(chunk):
    txt = "".join(chunk)
    n = max(len(txt), 1)
    quoted = sum(len(m) for m in re.findall(r"“[^”]*”", txt))
    dlg_lines = sum(1 for p in chunk if p.startswith("“") or p.startswith("「"))
    dlg = min(1.0, max(quoted / n * 1.6, dlg_lines / max(len(chunk), 1) * 1.2))

    # tanh 压缩：避免长片段天然计数高而把特征顶到 1
    act = math.tanh(sum(txt.count(v) for v in ACTION_VERBS) / n * 90)
    vis = math.tanh(sum(txt.count(w) for w in VISUAL_NOUNS) / n * 90)
    scene = math.tanh(sum(txt.count(m) for m in SCENE_MARKERS) / n * 260)
    mono = math.tanh(sum(txt.count(m) for m in MONOLOGUE_MARKERS) / n * 160)
    # 段落节奏：短段多 -> 更接近动作/对话场景
    para_len = statistics.mean(len(p) for p in chunk)
    rhythm = min(1.0, max(0.0, (55 - para_len) / 40)) if para_len < 55 else 0.0
    length_fit = math.exp(-((len(txt) - 900) ** 2) / (2 * 420 ** 2))

    f = {
        "dialogue": round(dlg, 3),
        "action": round(act, 3),
        "visual": round(vis, 3),
        "scene": round(scene, 3),
        "monologue": round(mono, 3),
        "rhythm": round(rhythm, 3),
        "length_fit": round(length_fit, 3),
        "chars": len(txt),
        "paras": len(chunk),
    }
    f["raw"] = round(0.9 * f["dialogue"] + 0.9 * f["action"] + 0.7 * f["visual"] + 1.1 * f["scene"]
                     + 0.7 * f["rhythm"] + 0.5 * f["length_fit"] - 1.2 * f["monologue"], 3)
    return f


def percentile_rank(values):
    """把原始分映射到 [0,1] 的语料内百分位，选择阈值不受尺度漂移影响。"""
    ordered = sorted(values)
    n = len(ordered)
    import bisect
    return [round((bisect.bisect_right(ordered, v) - 0.5) / n, 3) for v in values]


def main():
    txt_path = next(os.path.join(BOOK_DIR, f) for f in os.listdir(BOOK_DIR) if f.endswith(".txt"))
    text, enc = load_text(txt_path)
    chapters = split_chapters(text)

    segs = []
    for ci, ch in enumerate(chapters):
        for si, chunk in enumerate(segment_chapter(ch["paras"])):
            f = features(chunk)
            f.update(chapter=ci, seg=si, title=ch["title"], head=chunk[0][:60])
            segs.append(f)

    ranks = percentile_rank([s["raw"] for s in segs])
    for s, r in zip(segs, ranks):
        s["score"] = r

    scores = [s["score"] for s in segs]
    raws = [s["raw"] for s in segs]
    total_chars = sum(s["chars"] for s in segs)

    # 选片：每章保底 1 个，按百分位取 top-k；同章避免重复相邻窗口
    k_per_chapter = 1
    picked = []
    for ci in range(len(chapters)):
        cand = sorted([s for s in segs if s["chapter"] == ci], key=lambda x: -x["score"])
        chosen, used_seg = [], set()
        for c in cand:
            if len(chosen) >= k_per_chapter:
                break
            if any(abs(c["seg"] - u) == 0 for u in used_seg):
                continue
            chosen.append(c)
            used_seg.add(c["seg"])
        picked.extend(chosen)

    # LLM token 估算：中文约 0.75 token/字（含 JSON 指令开销另计）
    llm_chars = sum(s["chars"] for s in picked)
    report = {
        "book": os.path.basename(txt_path),
        "encoding": enc,
        "total_chars": len(text),
        "chapters": len(chapters),
        "avg_chapter_chars": round(statistics.mean(sum(len(p) for p in c["paras"]) for c in chapters)),
        "segments": len(segs),
        "avg_segment_chars": round(statistics.mean([s["chars"] for s in segs])),
        "raw_min": round(min(raws), 3),
        "raw_p50": round(statistics.median(raws), 3),
        "raw_p90": round(sorted(raws)[int(len(raws) * 0.9)], 3),
        "score_p50": round(statistics.median(scores), 3),
        "score_p90": round(sorted(scores)[int(len(scores) * 0.9)], 3),
        "score_max": round(max(scores), 3),
        "picked": len(picked),
        "picked_chars": llm_chars,
        "picked_char_ratio": round(llm_chars / total_chars, 3),
        "est_llm_input_tokens": int(llm_chars * 0.75),
        "pass_rate": round(len(picked) / len(segs), 3),
        "top_samples": sorted(picked, key=lambda x: -x["score"])[:8],
    }

    os.makedirs(OUT_DIR, exist_ok=True)
    with open(os.path.join(OUT_DIR, "spike_report.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(report, fh, ensure_ascii=False, indent=2)

    for k in ("book", "encoding", "total_chars", "chapters", "avg_chapter_chars", "segments",
              "avg_segment_chars", "raw_min", "raw_p50", "raw_p90", "score_p50", "score_p90", "score_max", "picked",
              "picked_chars", "picked_char_ratio", "est_llm_input_tokens", "pass_rate"):
        print(f"{k:22} {report[k]}")
    print("\ntop samples:")
    for s in report["top_samples"][:5]:
        print(f"  raw{s['raw']:+.2f} p{s['score']:.2f}  ch{s['chapter']+1:>3} seg{s['seg']}  {s['chars']}字  dlg{s['dialogue']} act{s['action']} scn{s['scene']} mono{s['monologue']}  | {s['head']}")


if __name__ == "__main__":
    main()
