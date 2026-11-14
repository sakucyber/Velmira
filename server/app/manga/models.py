from __future__ import annotations

import hashlib
import json
import math
from urllib.parse import urlsplit

from ..ai.models import identity_hash, stable_json

STYLE_PRESETS = {
    "anime": {"label": "日系二次元", "prompt": "Japanese anime-style full-color 2D manga panel, hand-drawn clean ink linework, expressive anime eyes, stylized facial features, refined cel shading, soft color gradients, layered illustrated hair highlights, cohesive character design, rich colors, readable silhouettes, dynamic manga composition"},
    "manga": {"label": "黑白漫画", "prompt": "black and white Japanese manga, crisp ink lines, screen tones, expressive lighting, dramatic comic composition"},
    "watercolor": {"label": "水彩插画", "prompt": "delicate watercolor illustration, soft pigment washes, textured paper, gentle color transitions, lyrical composition"},
    "chinese": {"label": "国风插画", "prompt": "Chinese fantasy illustration, elegant flowing brush lines, subtle ink wash textures, refined colors, atmospheric composition"},
    "semi_realistic": {"label": "半写实漫画", "prompt": "semi-realistic color comic illustration, detailed face, clean lines, cinematic lighting"},
    "cinematic": {"label": "电影写实", "prompt": "cinematic realistic illustration, natural skin texture, detailed materials, photographic lighting, film still composition"},
}

# High-level generation policies.  The detailed fields remain available for
# creators who need them, while the default settings page can expose one
# understandable choice instead of a wall of retry and validation numbers.
GENERATION_PRESETS = {
    "stable": {"label": "稳健", "description": "允许更长的图像请求时间，并保留人工复核。",
               "max_attempts": 3, "timeout": 900, "face_threshold": .60, "review_storyboards": True},
    "balanced": {"label": "平衡", "description": "适合日常生成的速度、重试和一致性设置。",
                 "max_attempts": 3, "timeout": 600, "face_threshold": .55, "review_storyboards": False},
    "fast": {"label": "激进", "description": "优先快速出草稿；低阈值结果仍需人工复核。",
             "max_attempts": 2, "timeout": 300, "face_threshold": .45, "review_storyboards": False},
}

GENERAL_NEGATIVE = "nsfw, lowres, worst quality, bad anatomy, bad hands, missing fingers, extra fingers, extra limbs, duplicated character, swapped face, inconsistent outfit, crowd, background people, blurry, jpeg artifacts, watermark, signature, embedded text, speech bubbles"
ANIME_EXCLUSIONS = "photorealistic, live action, photograph, realistic skin texture, skin pores, hyperrealism, 3D render, CGI, plastic skin"

DEFAULTS = {
    "provider": "comfyui", "base_url": "http://127.0.0.1:8188", "preset": "balanced",
    "model": "", "style_preset": "anime",
    "unet": "moss_image_sft_int8_convrot.safetensors",
    "clip": "moss\\qwen3.5_9b_int8_convrot.safetensors", "vae": "moss\\flux2-vae.safetensors",
    "edit_lora": "moss_image_edit_lora_bf16.safetensors",
    "width": 640, "height": 832, "steps": 20, "cfg": 2.5,
    "sampler": "er_sde", "scheduler": "simple", "seed": 424242,
    "style": STYLE_PRESETS["anime"]["prompt"],
    "negative": ANIME_EXCLUSIONS + ", " + GENERAL_NEGATIVE,
    "face_threshold": .55, "max_attempts": 3, "timeout": 600,
    "face_python": "", "face_models": "", "rpm": 25, "max_concurrency": 1, "review_storyboards": False, "reference_api": "edits",
}
SHOTS = {
    "portrait": "close-up portrait, eye-level, face clearly visible, frontal view",
    "medium": "medium shot, eye-level, face clearly visible",
    "wide": "wide shot, full body, environment visible",
    "three_quarter": "medium close-up, three-quarter view, face clearly visible",
    "reaction": "close-up reaction shot, expressive face",
    "detail": "detail shot focused on the relevant object or hand action",
}


class MangaError(ValueError):
    def __init__(self, message, code="invalid_render_request", status=400, *, status_code=None, error_code=None, reason=None):
        super().__init__(message)
        self.code, self.status = code, status
        self.status_code, self.error_code = status_code, error_code
        # A stable machine-readable cause for diagnostics.  Never present raw
        # provider text here; the analysis panel maps it to fixed copy.
        self.reason = reason


def digest(value):
    return hashlib.sha256(stable_json(value).encode()).hexdigest()


def validate_config(patch, old=None):
    if not isinstance(patch, dict) or set(patch) - set(DEFAULTS):
        raise MangaError("出图配置包含未知字段")
    preset = (patch or {}).get("preset", (old or {}).get("preset", "balanced"))
    if not isinstance(preset, str) or preset not in GENERATION_PRESETS:
        raise MangaError("未知生成策略预设")
    defaults = {k: v for k, v in GENERATION_PRESETS[preset].items() if k in DEFAULTS} if "preset" in patch else {}
    cfg = {**DEFAULTS, **(old or {}), **defaults, **patch, "preset": preset}
    if not isinstance(cfg["provider"], str) or cfg["provider"] not in {"comfyui", "mock", "openai-images"}:
        raise MangaError("图像后端须为 comfyui、openai-images 或 mock")
    if not isinstance(cfg["style_preset"], str) or cfg["style_preset"] not in {*STYLE_PRESETS, "custom"}:
        raise MangaError("未知画风预设")
    if "style_preset" in patch and cfg["style_preset"] != "custom":
        cfg["style"] = STYLE_PRESETS[cfg["style_preset"]]["prompt"]
    elif "style" in patch and cfg["style"] != STYLE_PRESETS.get(cfg["style_preset"], {}).get("prompt"):
        cfg["style_preset"] = "custom"
    if "negative" not in patch and ("style_preset" in patch or "style" in patch):
        inherited = (old or {}).get("negative", DEFAULTS["negative"])
        if inherited in (DEFAULTS["negative"], GENERAL_NEGATIVE):
            cfg["negative"] = DEFAULTS["negative"] if cfg["style_preset"] == "anime" else GENERAL_NEGATIVE
    if not isinstance(cfg["model"], str) or len(cfg["model"]) > 200:
        raise MangaError("生图模型配置无效")
    pool_enabled = False
    if cfg["provider"] == "openai-images":
        try:
            from ..ai.routing import is_enabled
            pool_enabled = is_enabled("image")
        except Exception:
            pool_enabled = False
        from ..ai.settings import validate_url
        if cfg["base_url"].strip():
            cfg["base_url"] = validate_url(cfg["base_url"])
        elif not pool_enabled:
            raise MangaError("请启用生图模型池并添加可用模型，或检查旧版生图配置")
        if not cfg["model"].strip() and not pool_enabled:
            raise MangaError("请启用生图模型池并添加可用模型，或检查旧版生图配置")
    if not isinstance(cfg["base_url"], str):
        raise MangaError("服务地址须为文本")
    try:
        url = urlsplit(cfg["base_url"]) if cfg["base_url"] else None
        port = url.port if url else None
    except ValueError as exc:
        raise MangaError("服务地址格式无效") from exc
    if cfg["base_url"] and (url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password or url.query or url.fragment):
        raise MangaError("服务地址须为不含凭据的 HTTP(S) 地址")
    cfg["base_url"] = cfg["base_url"].rstrip("/")
    if not isinstance(cfg["review_storyboards"], bool):
        raise MangaError("review_storyboards 须为布尔值")
    if cfg["reference_api"] not in ("edits", "generations"):
        raise MangaError("参考图接口须为 edits 或 generations")
    for key, low, high in [("rpm", 0, 1000), ("max_concurrency", 1, 8), ("width", 256, 1536), ("height", 256, 1536), ("steps", 1, 60), ("seed", 0, 2**53 - 1), ("max_attempts", 1, 3), ("timeout", 30, 3600)]:
        value = cfg[key]
        if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
            raise MangaError(f"{key} 须为 {low}–{high} 的整数")
    if cfg["width"] % 16 or cfg["height"] % 16:
        raise MangaError("宽高须为 16 的倍数")
    for key, low, high in [("cfg", 0, 15), ("face_threshold", 0, 1)]:
        value = cfg[key]
        if isinstance(value, bool) or not isinstance(value, (float, int)) or not math.isfinite(value) or not low <= value <= high:
            raise MangaError(f"{key} 超出有效范围")
    for key in ["unet", "clip", "vae", "edit_lora", "sampler", "scheduler", "style", "negative"]:
        required = cfg["provider"] != "openai-images" or key in {"style", "negative"}
        if not isinstance(cfg[key], str) or (required and not cfg[key].strip()) or len(cfg[key]) > 2000:
            raise MangaError(f"{key} 须为非空文本（最多 2000 字符）")
    if cfg["provider"] == "comfyui" and not ("9b" in cfg["clip"].lower()):
        raise MangaError("当前 MOSS 模板使用 9B 编码器；小编码器需要独立适配器模板")
    for key in ("face_python", "face_models"):
        if not isinstance(cfg[key], str) or len(cfg[key]) > 1000:
            raise MangaError(key + " 须为路径文本（最多 1000 字符）")
    return cfg


def check_identity(form):
    if not form or not form["locked"] or not form["identity_block"] or identity_hash(form["identity_block"]) != form["identity_hash"]:
        raise MangaError("形态身份须先冻结，且身份哈希校验通过", "identity_not_frozen", 409)


def form_order(form):
    # The initial base is a fallback when a timeline form starts at chapter zero.
    try:
        card = json.loads(form.get("card_json") or "{}")
    except (TypeError, ValueError):
        card = {}
    anchors = card.get("source_anchors") or ([card["source_anchor"]] if isinstance(card.get("source_anchor"), dict) else [])
    latest = max(((_anchor_position(anchor)) for anchor in anchors if isinstance(anchor, dict)), default=(-1, -1, -1))
    return (form["start_ch"], form["start_para"], form.get("form_key") != "base", latest, form["id"])


def _anchor_position(anchor):
    """Comparable chapter/paragraph/character position for a form occurrence."""
    chapter = anchor.get("chapter", -1) if type(anchor.get("chapter", -1)) is int else -1
    offset = anchor.get("offset", -1) if type(anchor.get("offset", -1)) is int else -1
    para = anchor.get("para", -1) if type(anchor.get("para", -1)) is int else -1
    return (chapter, offset, para)


def _form_occurrences(form):
    try:
        card = json.loads(form.get("card_json") or "{}")
    except (TypeError, ValueError):
        card = {}
    values = card.get("source_anchors")
    if not isinstance(values, list):
        values = [card.get("source_anchor")] if isinstance(card.get("source_anchor"), dict) else []
    elif isinstance(card.get("source_anchor"), dict):
        values = [card["source_anchor"], *values]
    return [anchor for anchor in values if isinstance(anchor, dict) and isinstance(anchor.get("chapter"), int)
            and isinstance(anchor.get("offset", -1), int)]


def resolve_form(forms, chapter, para, explicit=None, flashback=False, source_offset=None):
    if source_offset is not None:
        def active_at(form):
            anchors = _form_occurrences(form)
            return not anchors or any(_anchor_position(anchor) <= (chapter, source_offset, para) for anchor in anchors)
        forms = [form for form in forms if active_at(form)]
    ordered = sorted(forms, key=form_order)
    available = [f for f in ordered if (f["start_ch"], f["start_para"]) <= (chapter, para)]
    if not available:
        raise MangaError("该段落还没有生效的人物形态", "form_not_active", 409)
    if source_offset is not None:
        def effective_order(form):
            prior = [anchor for anchor in _form_occurrences(form)
                     if _anchor_position(anchor) <= (chapter, source_offset, para)]
            if prior:
                anchor = max(prior, key=_anchor_position)
                return (anchor["chapter"], anchor.get("para", form["start_para"]), anchor.get("offset", -1), form.get("form_key") != "base", form["id"])
            return (form["start_ch"], form["start_para"], -1, form.get("form_key") != "base", form["id"])
        active = max(available, key=effective_order)
    else:
        # Paragraph-only callers use the latest occurrence in that paragraph;
        # exact comic panels always supply source_offset for finer ordering.
        def paragraph_order(form):
            past = [anchor for anchor in _form_occurrences(form)
                    if (anchor["chapter"], anchor.get("para", form["start_para"])) <= (chapter, para)]
            if past:
                anchor = max(past, key=_anchor_position)
                return (anchor["chapter"], anchor.get("para", form["start_para"]), form.get("form_key") != "base", anchor.get("offset", -1), form["id"])
            return (form["start_ch"], form["start_para"], form.get("form_key") != "base", -1, form["id"])
        active = max(available, key=paragraph_order)
    if explicit:
        selected = next((f for f in ordered if f["id"] == explicit), None)
        if not selected:
            raise MangaError("指定形态不属于该人物", "form_not_found", 404)
        if (selected["start_ch"], selected["start_para"]) > (chapter, para):
            raise MangaError("该形态尚未在此段落生效", "form_not_active", 409)
        if selected["id"] != active["id"] and not flashback:
            raise MangaError("形态与段落时间轴冲突；回忆场景须显式标记闪回", "form_regression", 409)
        active = selected
    check_identity(active)
    return active


def build_prompt(config, identity, scene, shot, action, reference=False, visual_prompt=None):
    if shot not in SHOTS:
        raise MangaError("不支持的镜头")
    sections = [config["style"], identity]
    if config.get("style_preset") == "anime":
        sections.append("strictly 2D illustrated anime manga, not photorealistic, not live action, not 3D, clean line art and cel shading")
    if reference:
        sections.append("same character and face as reference, new pose and shot, preserve identity")
    if visual_prompt:
        sections.extend([visual_prompt, SHOTS[shot]])
    else:
        sections.extend([str(scene.get("location", "")), str(scene.get("time_of_day", "")),
                         str(scene.get("summary", "")), SHOTS[shot], action,
                         ", ".join(map(str, scene.get("key_props", [])))])
    sections.append("single coherent comic panel, no embedded text, no speech bubbles, no watermark, solo, exactly one person, no other people in background")
    prompt = ". ".join(s.strip() for s in sections if s and s.strip())
    if len(prompt) > 4000:
        raise MangaError("提示词过长，请精简场景或风格")
    return prompt


def build_workflow(cfg, prompt, seed, reference=None, prefix="velmira"):
    graph = {
        "1": {"class_type": "UNETLoader", "inputs": {"unet_name": cfg["unet"], "weight_dtype": "default"}},
        "2": {"class_type": "CLIPLoader", "inputs": {"clip_name": cfg["clip"], "type": "moss_image", "device": "default"}},
        "3": {"class_type": "VAELoader", "inputs": {"vae_name": cfg["vae"]}},
        "4": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["2", 0], "text": prompt}},
        "5": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["2", 0], "text": cfg["negative"]}},
        "6": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": cfg["width"], "height": cfg["height"], "batch_size": 1}},
        "7": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "positive": ["4", 0], "negative": ["5", 0], "latent_image": ["6", 0], "seed": seed, "steps": cfg["steps"], "cfg": cfg["cfg"], "sampler_name": cfg["sampler"], "scheduler": cfg["scheduler"], "denoise": 1.0}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3", 0]}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": prefix}},
    }
    if reference:
        graph["10"] = {"class_type": "LoadImage", "inputs": {"image": reference}}
        graph["11"] = {"class_type": "ImageScale", "inputs": {"image": ["10", 0], "upscale_method": "lanczos", "width": cfg["width"], "height": cfg["height"], "crop": "center"}}
        graph["12"] = {"class_type": "LoraLoaderModelOnly", "inputs": {"model": ["1", 0], "lora_name": cfg["edit_lora"], "strength_model": 1.0}}
        graph["7"]["inputs"]["model"] = ["12", 0]
        for node, text in [("4", prompt), ("5", cfg["negative"])]:
            graph[node] = {"class_type": "TextEncodeMOSSImageEdit", "inputs": {"clip": ["2", 0], "prompt": text, "vae": ["3", 0], "edit_format": "Moss-Image-Edit", "images.image_0": ["11", 0], "image_strength": 1.0}}
    return graph
