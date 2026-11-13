from __future__ import annotations

import hashlib
import math
from urllib.parse import urlsplit

from ..ai.models import identity_hash, stable_json

STYLE_PRESETS = {
    "anime": {"label": "日系二次元", "prompt": "Japanese anime illustration, expressive anime eyes, refined cel shading, clean line art, rich colors, cinematic composition"},
    "manga": {"label": "黑白漫画", "prompt": "black and white Japanese manga, crisp ink lines, screen tones, expressive lighting, dramatic comic composition"},
    "watercolor": {"label": "水彩插画", "prompt": "delicate watercolor illustration, soft pigment washes, textured paper, gentle color transitions, lyrical composition"},
    "chinese": {"label": "国风插画", "prompt": "Chinese fantasy illustration, elegant flowing brush lines, subtle ink wash textures, refined colors, atmospheric composition"},
    "semi_realistic": {"label": "半写实漫画", "prompt": "semi-realistic color comic illustration, detailed face, clean lines, cinematic lighting"},
    "cinematic": {"label": "电影写实", "prompt": "cinematic realistic illustration, natural skin texture, detailed materials, photographic lighting, film still composition"},
}

DEFAULTS = {
    "provider": "comfyui", "base_url": "http://127.0.0.1:8188",
    "model": "", "style_preset": "anime",
    "unet": "moss_image_sft_int8_convrot.safetensors",
    "clip": "moss\\qwen3.5_9b_int8_convrot.safetensors", "vae": "moss\\flux2-vae.safetensors",
    "edit_lora": "moss_image_edit_lora_bf16.safetensors",
    "width": 640, "height": 832, "steps": 20, "cfg": 2.5,
    "sampler": "er_sde", "scheduler": "simple", "seed": 424242,
    "style": STYLE_PRESETS["anime"]["prompt"],
    "negative": "nsfw, worst quality, bad anatomy, watermark, text, speech bubbles, extra fingers, duplicate person, crowd, background people, additional faces",
    "face_threshold": .55, "max_attempts": 3, "timeout": 600,
    "face_python": "", "face_models": "",
}
SHOTS = {
    "portrait": "close-up portrait, eye-level, face clearly visible, frontal view",
    "medium": "medium shot, eye-level, face clearly visible",
    "wide": "wide shot, full body, environment visible",
    "three_quarter": "medium close-up, three-quarter view, face clearly visible",
    "reaction": "close-up reaction shot, expressive face",
}


class MangaError(ValueError):
    def __init__(self, message, code="invalid_render_request", status=400):
        super().__init__(message)
        self.code, self.status = code, status


def digest(value):
    return hashlib.sha256(stable_json(value).encode()).hexdigest()


def validate_config(patch, old=None):
    if not isinstance(patch, dict) or set(patch) - set(DEFAULTS):
        raise MangaError("出图配置包含未知字段")
    cfg = {**DEFAULTS, **(old or {}), **patch}
    if not isinstance(cfg["provider"], str) or cfg["provider"] not in {"comfyui", "mock", "openai-images"}:
        raise MangaError("图像后端须为 comfyui、openai-images 或 mock")
    if not isinstance(cfg["style_preset"], str) or cfg["style_preset"] not in {*STYLE_PRESETS, "custom"}:
        raise MangaError("未知画风预设")
    if "style_preset" in patch and cfg["style_preset"] != "custom":
        cfg["style"] = STYLE_PRESETS[cfg["style_preset"]]["prompt"]
    elif "style" in patch and cfg["style"] != STYLE_PRESETS.get(cfg["style_preset"], {}).get("prompt"):
        cfg["style_preset"] = "custom"
    if not isinstance(cfg["model"], str) or len(cfg["model"]) > 200:
        raise MangaError("图像 API 模型名称无效")
    if cfg["provider"] == "openai-images":
        from ..ai.settings import validate_url
        cfg["base_url"] = validate_url(cfg["base_url"])
        if not cfg["model"].strip():
            raise MangaError("请填写图像 API 模型名称")
    if not isinstance(cfg["base_url"], str):
        raise MangaError("服务地址须为文本")
    try:
        url = urlsplit(cfg["base_url"])
        port = url.port
    except ValueError as exc:
        raise MangaError("服务地址格式无效") from exc
    if url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise MangaError("服务地址须为不含凭据的 HTTP(S) 地址")
    cfg["base_url"] = cfg["base_url"].rstrip("/")
    for key, low, high in [("width", 256, 1536), ("height", 256, 1536), ("steps", 1, 60), ("seed", 0, 2**53 - 1), ("max_attempts", 1, 3), ("timeout", 30, 3600)]:
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


def resolve_form(forms, chapter, para, explicit=None, flashback=False):
    ordered = sorted(forms, key=lambda f: (f["start_ch"], f["start_para"], f["id"]))
    available = [f for f in ordered if (f["start_ch"], f["start_para"]) <= (chapter, para)]
    if not available:
        raise MangaError("该段落还没有生效的人物形态", "form_not_active", 409)
    active = available[-1]
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
    if reference:
        sections.append("same character and face as reference, new pose and shot, preserve identity")
    if visual_prompt:
        sections.extend([visual_prompt, SHOTS[shot]])
    else:
        sections.extend([str(scene.get("location", "")), str(scene.get("time_of_day", "")),
                         str(scene.get("summary", "")), SHOTS[shot], action,
                         ", ".join(map(str, scene.get("key_props", [])))])
    sections.append("single comic panel, no text, solo, exactly one person, no other people in background")
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
