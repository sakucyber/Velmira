from __future__ import annotations

import io
import base64
import hashlib
import json
import urllib.error
import urllib.parse
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor

from PIL import Image, ImageDraw

from .models import MangaError, build_workflow


class ImageAPIProvider:
    """OpenAI Images-compatible adapter. Uses edits for panels with a reference."""
    def __init__(self, config, *, credential_slot="image"):
        self.config = config
        self.credential_slot = credential_slot

    def probe(self):
        from ..ai import connections, routing
        from ..ai.settings import api_key_for
        from ..ai.providers import request_json
        try:
            if self.credential_slot == "image" and routing.is_enabled("image"):
                pool = routing.get_config("image")
                def check(candidate):
                    result = {"connection_id": candidate["connection_id"], "model": candidate["model"], "ok": False}
                    try:
                        cfg = connections.resolve(candidate["connection_id"], candidate["model"], "image")
                        data = request_json(cfg["base_url"] + "/models",
                                            api_key=api_key_for(cfg["base_url"], cfg["credential_slot"]),
                                            timeout=min(pool["timeout"], 10))
                        result["ok"] = any(m.get("id") == cfg["model"] for m in data.get("data", []) if isinstance(m, dict))
                    except Exception:
                        # Probe errors stay local to this candidate and never
                        # affect production cooldowns, routing counts or keys.
                        pass
                    return result
                with ThreadPoolExecutor(max_workers=min(8, max(1, len(pool["models"])))) as workers:
                    models = list(workers.map(check, pool["models"]))
                found = any(model["ok"] for model in models)
                return {"ok": found, "provider": "openai-images", "reference_mode": "model-pool",
                        "missing": [] if found else ["models"], "models": models,
                        "note": "模型池中可访问的模型：" + str(sum(model["ok"] for model in models)) + "/" + str(len(models)) +
                                "；仅验证模型列表，实际生图及参考图能力会在生成时验证"}
            data = request_json(self.config["base_url"] + "/models", api_key=api_key_for(self.config["base_url"], self.credential_slot))
            found = any(m.get("id") == self.config["model"] for m in data.get("data", []) if isinstance(m, dict))
            return {"ok": found, "provider": "openai-images", "missing": [] if found else ["model"],
                    "note": "模型可访问；生图与参考图编辑能力会在实际请求时验证", "reference_mode": "images-" + self.config.get("reference_api", "edits")}
        except Exception:
            raise MangaError("生图模型池检测失败，请检查连接、模型和密钥", "image_api_failed", 502) from None

    def generate(self, snapshot, ref_path, request_id):
        from ..ai import routing
        from ..ai.providers import ProviderRequestError
        try:
            if self.credential_slot == "image" and routing.is_enabled("image"):
                def invoke(candidate):
                    # Preserve scene dimensions/prompt/references while swapping only the connection.
                    cfg = {**self.config, **candidate}
                    return self._generate_once(snapshot, ref_path, request_id, cfg)
                return routing.run("image", invoke)
            return self._generate_once(snapshot, ref_path, request_id,
                                       {**self.config, "credential_slot": self.credential_slot})
        except routing.RoutingExhausted as exc:
            raise MangaError(str(exc), "image_routing_exhausted", 502,
                             status_code=exc.status_code, error_code=exc.error_code) from None
        except ProviderRequestError as exc:
            raise MangaError(str(exc), "image_api_failed", 502,
                             status_code=exc.status_code, error_code=exc.error_code) from None
        except Exception:
            raise MangaError("图像 API 未返回有效图片，或连接中断；请检查接口兼容性与服务端任务记录", "image_api_failed", 502) from None

    def _generate_once(self, snapshot, ref_path, request_id, cfg):
        from ..ai.settings import api_key_for
        from ..ai.providers import _NoRedirect, ProviderRequestError
        from ..ai.limits import request_slot, RequestSlotBusy
        # OpenAI-compatible Images APIs generally have no negative_prompt field.
        # Carry the negative constraints in the actual transmitted prompt too.
        prompt = snapshot["prompt"]
        if cfg.get("negative", "").strip():
            prompt += "\nVisual exclusions: " + cfg["negative"].strip()
        body = {"model": cfg["model"], "prompt": prompt, "n": 1, "size": f'{cfg["width"]}x{cfg["height"]}'}
        if not cfg["model"].lower().startswith("gpt-image"):
            body["response_format"] = "b64_json"
        if ref_path and cfg.get("reference_api", "edits") == "generations":
            paths = ref_path if isinstance(ref_path, list) else [ref_path]
            body["image"] = ["data:image/png;base64," + base64.b64encode(path.read_bytes()).decode("ascii") for path in paths]
            raw = json.dumps(body).encode(); content_type = "application/json"; endpoint = "/images/generations"
        elif ref_path:
            boundary = "velmira" + uuid.uuid4().hex
            chunks = [(f'--{boundary}\r\nContent-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n').encode() for key, value in body.items()]
            paths = ref_path if isinstance(ref_path, list) else [ref_path]
            field = "image[]" if len(paths) > 1 else "image"
            for index, path in enumerate(paths):
                chunks.append((f'--{boundary}\r\nContent-Disposition: form-data; name="{field}"; filename="reference-{index + 1}.png"\r\nContent-Type: image/png\r\n\r\n').encode() + path.read_bytes() + b"\r\n")
            chunks.append(f"--{boundary}--\r\n".encode())
            raw = b"".join(chunks); content_type = "multipart/form-data; boundary=" + boundary
            endpoint = "/images/edits"
        else:
            raw = json.dumps(body).encode(); content_type = "application/json"; endpoint = "/images/generations"
        try:
            key = api_key_for(cfg["base_url"], cfg.get("credential_slot", self.credential_slot))
            if cfg.get("_route_id"):
                # Models on the same endpoint need distinct, retry-stable idempotency keys.
                suffix = hashlib.sha256(str(cfg["_route_id"]).encode()).hexdigest()[:16]
                request_id = request_id + "-" + suffix
            req = urllib.request.Request(cfg["base_url"] + endpoint, data=raw,
                headers={"Content-Type": content_type, "Idempotency-Key": request_id})
            if key:
                req.add_header("Authorization", "Bearer " + key)
            with request_slot((cfg["base_url"], "image"), cfg.get("rpm", 25), cfg.get("max_concurrency", 1),
                              wait_timeout=0 if cfg.get("_route_id") else None):
                with urllib.request.build_opener(_NoRedirect()).open(req, timeout=cfg["timeout"]) as response:
                    data = response.read(24 * 1024 * 1024 + 1)
                    if len(data) > 24 * 1024 * 1024:
                        raise ValueError()
            payload = json.loads(data)
            encoded = payload["data"][0]["b64_json"]
            image = base64.b64decode(encoded, validate=True)
            if len(image) > 16 * 1024 * 1024:
                raise ValueError()
            try:
                with Image.open(io.BytesIO(image)) as decoded:
                    if not 32 <= decoded.width <= 4096 or not 32 <= decoded.height <= 4096:
                        raise ValueError()
                    # Match save_image's full decoding before routing records a
                    # success, so malformed/unsupported output can fall back.
                    decoded.load()
                    decoded.convert("RGB")
            except Exception:
                raise ValueError() from None
            return image
        except urllib.error.HTTPError as exc:
            raise ProviderRequestError(f"图像 API HTTP {exc.code}；请确认模型支持所选尺寸及参考图编辑",
                                       status_code=exc.code, error_code="http_error") from None
        except RequestSlotBusy:
            raise
        except (TimeoutError, urllib.error.URLError, OSError) as exc:
            timed_out = isinstance(exc, TimeoutError) or isinstance(getattr(exc, "reason", None), TimeoutError)
            raise ProviderRequestError("图像 API 请求超时" if timed_out else "图像 API 连接失败",
                                       error_code="timeout" if timed_out else "connection_error") from None
        except Exception:
            raise ProviderRequestError("图像 API 未返回有效 base64 图片；请检查接口兼容性与服务端任务记录",
                                       error_code="invalid_response") from None


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
            raise MangaError(f"ComfyUI HTTP {exc.code}，请检查服务端任务记录", "comfy_error", 502,
                             status_code=exc.code, error_code="http_error") from None
        except (OSError, ValueError) as exc:
            raise MangaError("无法连接图像服务，请检查连接", "comfy_unavailable", 503,
                             error_code="connection_error") from None

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
            raise MangaError("节点或模型配置错误，请检查 ComfyUI", "invalid_workflow", 502)
        return result["prompt_id"], graph

    def result(self, prompt_id):
        history = self.request("/history/" + urllib.parse.quote(prompt_id, safe=""))
        item = history.get(prompt_id)
        if not item:
            return None
        if item.get("status", {}).get("status_str") == "error":
            raise MangaError("ComfyUI 执行失败，请检查服务端任务记录", "render_failed", 502)
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
