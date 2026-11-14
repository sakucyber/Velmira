"""M4 page geometry and Chinese typesetting. Text is drawn after image generation."""
from __future__ import annotations

import os
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps

from .models import MangaError

# Each layout's value is its exact panel capacity.  The asymmetric layouts
# are deliberate: auto pages must never render an empty placeholder cell.
LAYOUTS = {
    "1x1": 1,
    "2x1": 2,
    "1+2": 3,
    "2+1": 3,
    "1+2+2": 5,
    "2+2+1": 5,
    "2x2": 4,
    "2+3": 5,
    "2x3": 6,
    "3x2": 6,
    "3+4": 7,
    "2+2+2+1": 7,
}
DEFAULTS = {"layout": "auto", "width": 1200, "height": 1800, "gutter": 8,
            "font": "sans", "font_size": 32, "title": "", "panels": []}


def integer(value, low, high, label):
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise MangaError(f"{label} 须为 {low}–{high} 的整数")
    return value


def text(value, limit, label):
    if not isinstance(value, str) or len(value) > limit or any(ord(c) < 32 and c not in "\n\t" for c in value):
        raise MangaError(f"{label} 须为最多 {limit} 字符的文本")
    return value.strip()


def validate(payload):
    if not isinstance(payload, dict) or set(payload) - set(DEFAULTS):
        raise MangaError("拼页设置包含未知字段")
    spec = {**DEFAULTS, **payload}
    if not isinstance(spec["layout"], str) or spec["layout"] not in {*LAYOUTS, "auto"}:
        raise MangaError("未知拼页模板")
    for key, low, high in [("width", 640, 2400), ("height", 960, 3600), ("gutter", 4, 48), ("font_size", 18, 72)]:
        integer(spec[key], low, high, key)
    if spec["font"] not in ("sans", "serif"):
        raise MangaError("字体须为 sans 或 serif")
    spec["title"] = text(spec["title"], 100, "页标题")
    if not isinstance(spec["panels"], list) or not 1 <= len(spec["panels"]) <= 7:
        raise MangaError("一页须包含 1–7 格")
    panels = []
    for p in spec["panels"]:
        if not isinstance(p, dict) or set(p) - {"job_id", "caption", "dialogue", "speaker", "bubble", "align", "fit", "source_span"}:
            raise MangaError("分格内容格式无效")
        p = {"caption": "", "dialogue": "", "speaker": "", "bubble": "rounded", "align": "left", "fit": "contain", "source_span": None, **p}
        for key, limit in [("job_id", 100), ("caption", 300), ("dialogue", 400), ("speaker", 40)]:
            p[key] = text(p.get(key), limit, key)
        if not p["job_id"] or p["bubble"] not in ("rounded", "ellipse") or p["align"] not in ("left", "right") or p["fit"] not in ("contain", "cover"):
            raise MangaError("分格图像、气泡或适配方式无效")
        span = p["source_span"]
        if span is not None:
            if not isinstance(span, dict) or set(span) - {"chapter", "start", "end", "start_para", "start_offset", "end_para", "end_offset", "quote"}:
                raise MangaError("分格原文范围格式无效")
            for key in ("chapter", "start", "end", "start_para", "start_offset", "end_para", "end_offset"):
                if key in span and (isinstance(span[key], bool) or not isinstance(span[key], int) or span[key] < 0):
                    raise MangaError("分格原文位置无效")
            if "start" in span and "end" in span and span["end"] < span["start"]:
                raise MangaError("分格原文范围倒置")
            if "quote" in span and (not isinstance(span["quote"], str) or len(span["quote"]) > 20000):
                raise MangaError("分格原文摘录无效")
        panels.append(p)
    spec["panels"] = panels
    if spec["layout"] == "auto":
        spec["layout"] = {1: "1x1", 2: "2x1", 3: "1+2", 4: "2x2", 5: "1+2+2", 6: "2x3", 7: "2+2+2+1"}[len(panels)]
    if len(panels) > LAYOUTS[spec["layout"]]:
        raise MangaError("分格数量超过当前模板，请切换模板或移除分格")
    return spec


def font_path(family):
    override = os.environ.get("VELMIRA_COMIC_FONT_" + family.upper())
    windows = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"
    candidates = ([Path(override)] if override else []) + [
        windows / ("msyh.ttc" if family == "sans" else "simsun.ttc"),
        Path("/System/Library/Fonts/PingFang.ttc"),
        Path("/usr/share/fonts/opentype/noto/" + ("NotoSansCJK-Regular.ttc" if family == "sans" else "NotoSerifCJK-Regular.ttc")),
        Path("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc"),
    ]
    found = next((p for p in candidates if p.is_file()), None)
    if not found:
        raise MangaError("未找到中文字体，请安装 Noto CJK，或设置 VELMIRA_COMIC_FONT_SANS / SERIF", "font_unavailable", 409)
    return found


def font(family, size):
    return ImageFont.truetype(str(font_path(family)), size)


def wrap(text, face, width):
    """Pixel-based wrapping for Chinese and long Latin strings; preserve explicit breaks."""
    lines = []
    closing = set("，。！？、；：）》】」』”’,.!?;:%")
    opening = set("（《【「『“‘(")
    for paragraph in text.split("\n"):
        line = ""
        for char in paragraph.replace("\t", "  "):
            if line and face.getlength(line + char) > width:
                if char in closing and len(line) > 1:
                    lines.append(line[:-1]); line = line[-1] + char
                elif line[-1] in opening and len(line) > 1:
                    lines.append(line[:-1]); line = line[-1] + char
                else:
                    lines.append(line); line = char
            else:
                line += char
        lines.append(line)
    return lines


def rectangles(spec):
    margin = 24
    top = margin + (spec["font_size"] * 2 if spec["title"] else 0)
    left, right, bottom = margin, spec["width"] - margin, spec["height"] - margin
    gap = spec["gutter"]
    layout = spec["layout"]
    if "+" in layout:
        rows = list(map(int, layout.split("+")))
    else:
        cols, count = map(int, layout.split("x")); rows = [cols] * count
    rh = (bottom - top - gap * (len(rows) - 1)) / len(rows)
    result = []
    for row, columns in enumerate(rows):
        cw = (right - left - gap * (columns - 1)) / columns
        for col in range(columns):
            x, y = round(left + col * (cw + gap)), round(top + row * (rh + gap))
            result.append((x, y, round(left + col * (cw + gap) + cw), round(top + row * (rh + gap) + rh)))
    return result


def typeset(panel, spec, width, height):
    dialogue = (panel["speaker"] + "：" if panel["speaker"] else "") + panel["dialogue"] if panel["dialogue"] else ""
    for size in range(spec["font_size"], 17, -1):
        face = font(spec["font"], size)
        line_height = round(size * 1.45)
        cap = wrap(panel["caption"], face, width - 36) if panel["caption"] else []
        # Ellipse text sits inside its inscribed rectangle, away from the curved border.
        bubble_width = round((width - 24) * (.72 if panel["bubble"] == "ellipse" else .9))
        speech = wrap(dialogue, face, bubble_width - 40) if dialogue else []
        cap_height = len(cap) * line_height + 16 if cap else 0
        speech_height = round((len(speech) * line_height + 28) * (1.5 if panel["bubble"] == "ellipse" else 1)) + 12 if speech else 0
        if cap_height + speech_height <= height * .46:
            return face, line_height, cap, speech, cap_height, speech_height
    raise MangaError("文字放不下当前分格，请缩短旁白/对白或换更大的模板", "text_overflow", 409)


def compose(spec, sources, destination):
    canvas = Image.new("RGB", (spec["width"], spec["height"]), "white")
    draw = ImageDraw.Draw(canvas)
    if spec["title"]:
        face = font(spec["font"], spec["font_size"])
        if face.getlength(spec["title"]) > spec["width"] - 48:
            raise MangaError("页标题过长，请缩短标题")
        draw.text((24, 22), spec["title"], font=face, fill="#202020", anchor="lt")
    for panel, source, box in zip(spec["panels"], sources, rectangles(spec)):
        x, y, right, bottom = box
        width, height = right - x, bottom - y
        face, line_h, cap, speech, cap_h, speech_h = typeset(panel, spec, width, height)
        art_h = height - cap_h - speech_h - 4
        with Image.open(source) as original:
            original = ImageOps.exif_transpose(original).convert("RGB")
            art = ImageOps.fit(original, (width - 4, art_h), method=Image.Resampling.LANCZOS) if panel["fit"] == "cover" else ImageOps.contain(original, (width - 4, art_h), method=Image.Resampling.LANCZOS)
            canvas.paste(art, (x + (width - art.width) // 2, y + 2 + (art_h - art.height) // 2))
        cy = y + art_h + 8
        for line in cap:
            draw.text((x + 18, cy), line, font=face, fill="#202020", anchor="lt"); cy += line_h
        if cap:
            cy += 8
        if speech:
            bx0, bx1 = x + 12, right - 12
            by0, by1 = cy + 10, bottom - 8
            tail_x = bx0 + 38 if panel["align"] == "left" else bx1 - 38
            draw.polygon([(tail_x - 10, by0 + 10), (tail_x, by0 - 9), (tail_x + 14, by0 + 10)], fill="white", outline="#202020")
            if panel["bubble"] == "ellipse":
                draw.ellipse((bx0, by0, bx1, by1), fill="white", outline="#202020", width=2)
            else:
                draw.rounded_rectangle((bx0, by0, bx1, by1), radius=18, fill="white", outline="#202020", width=2)
            ty = by0 + (by1 - by0 - len(speech) * line_h) / 2 + 3
            for line in speech:
                draw.text(((bx0 + bx1 - face.getlength(line)) / 2, ty), line, font=face, fill="#202020", anchor="lt"); ty += line_h
        # Clip the entire panel, including cover-fitted artwork, to rounded corners.
        patch = canvas.crop(box)
        mask = Image.new("L", (width, height), 0)
        ImageDraw.Draw(mask).rounded_rectangle((0, 0, width - 1, height - 1), radius=10, fill=255)
        draw.rectangle(box, fill="white")
        canvas.paste(patch, (x, y), mask)
        draw.rounded_rectangle((x, y, right - 1, bottom - 1), radius=10, outline="#282828", width=2)
    destination.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(destination, "PNG")
