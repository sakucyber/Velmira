from __future__ import annotations

import io
import json
import urllib.error
import urllib.parse
import urllib.request
import uuid

from PIL import Image, ImageDraw

from .models import MangaError, build_workflow


class ComfyProvider:
    def __init__(self, config):
        self.config = config
        # Local ComfyUI must not be sent through system HTTP proxies.
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(self, path, data=None, headers=None):
        if isinstance(data, dict):
            data = json.dumps(data).encode()
            headers = {"Content-Type": "application/json"}
        req = urllib.request.Request(self.config["base_url"] + path, data=data, headers=headers or {})
        try:
            with self.http.open(req, timeout=60) as response:
                raw = response.read(32 * 1024 * 1024 + 1)
                if len(raw) > 32 * 1024 * 1024:
                    raise MangaError("图像服务响应超过 32 MB")
                if path.startswith("/view?"):
                    return raw
                return json.loads(raw)
        except urllib.error.HTTPError as exc:
            detail = exc.read(2000).decode("utf-8", "replace")
            raise MangaError(f"ComfyUI HTTP {exc.code}: {detail}", "comfy_error", 502) from exc
        except (OSError, ValueError) as exc:
            raise MangaError(f"无法连接图像服务：{exc}", "comfy_unavailable", 503) from exc

    def probe(self):
        info = self.request("/object_info")
        required = {"UNETLoader", "CLIPLoader", "VAELoader", "KSampler", "CLIPTextEncode", "EmptyFlux2LatentImage", "VAEDecode", "SaveImage", "LoadImage", "ImageScale", "LoraLoaderModelOnly", "TextEncodeMOSSImageEdit"}
        missing = sorted(required - set(info))
        models = {}
        for node, field, key in [("UNETLoader", "unet_name", "unet"), ("CLIPLoader", "clip_name", "clip"), ("VAELoader", "vae_name", "vae"), ("LoraLoaderModelOnly", "lora_name", "edit_lora")]:
            choices = info.get(node, {}).get("input", {}).get("required", {}).get(field, [[]])[0]
            models[key] = choices if isinstance(choices, list) else []
            if self.config[key] not in models[key]:
                missing.append("model:" + key)
        return {"ok": not missing, "provider": "comfyui", "missing": missing, "models": models, "reference_mode": "moss-edit-single"}

    def submit(self, snapshot, seed, ref_path, prefix, before_submit=None):
        ref_name = None
        if ref_path:
            boundary = "velmira" + uuid.uuid4().hex
            data = (f'--{boundary}\r\nContent-Disposition: form-data; name="image"; filename="{uuid.uuid4().hex}.png"\r\nContent-Type: image/png\r\n\r\n').encode() + ref_path.read_bytes() + f'\r\n--{boundary}--\r\n'.encode()
            uploaded = self.request("/upload/image", data, {"Content-Type": "multipart/form-data; boundary=" + boundary})
            ref_name = "/".join(x for x in [uploaded.get("subfolder", ""), uploaded["name"]] if x)
        graph = build_workflow(self.config, snapshot["prompt"], seed, ref_name, prefix)
        prompt_id = str(uuid.uuid4())
        if before_submit:
            before_submit(prompt_id, graph)
        result = self.request("/prompt", {"prompt": graph, "client_id": "velmira-m3", "prompt_id": prompt_id})
        if result.get("node_errors") or not result.get("prompt_id"):
            raise MangaError("节点或模型配置错误：" + str(result.get("node_errors", result))[:1500], "invalid_workflow", 502)
        return result["prompt_id"], graph

    def result(self, prompt_id):
        history = self.request("/history/" + urllib.parse.quote(prompt_id, safe=""))
        item = history.get(prompt_id)
        if not item:
            return None
        if item.get("status", {}).get("status_str") == "error":
            raise MangaError("ComfyUI 执行失败：" + str(item["status"].get("messages", []))[-1500:], "render_failed", 502)
        images = item.get("outputs", {}).get("9", {}).get("images", [])
        if not images:
            raise MangaError("任务完成但没有输出图片", "image_missing", 502)
        query = urllib.parse.urlencode({k: images[0].get(k, "output" if k == "type" else "") for k in ["filename", "subfolder", "type"]})
        return self.request("/view?" + query)


def mock_image(seed, width, height):
    image = Image.new("RGB", (width, height), (40 + seed % 120, 65, 95))
    draw = ImageDraw.Draw(image)
    draw.rectangle((width // 5, height // 5, width * 4 // 5, height * 4 // 5), outline="white", width=4)
    draw.text((20, 20), "MOCK - layout only, no identity validation", fill="white")
    out = io.BytesIO(); image.save(out, format="PNG")
    return out.getvalue()
