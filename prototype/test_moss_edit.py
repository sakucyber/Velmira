"""验证 MOSS 多参考图编辑（变身切换的地基）
两张不同角色参考图 -> 编辑指令，看是否同时生效且不串。
"""
import json
import time
import urllib.request
import urllib.parse

BASE = "http://127.0.0.1:8188"
W, H = 640, 832
STEPS, CFG, SEED = 20, 2.5, 424242

graph = {
    "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "moss_image_sft_int8_convrot.safetensors", "weight_dtype": "default"}},
    "2": {"class_type": "CLIPLoader", "inputs": {"clip_name": "moss\\qwen3.5_9b_int8_convrot.safetensors", "type": "moss_image", "device": "default"}},
    "3": {"class_type": "VAELoader", "inputs": {"vae_name": "moss\\flux2-vae.safetensors"}},
    "4": {"class_type": "LoadImage", "inputs": {"image": "anchor_claude.png"}},
    "5": {"class_type": "LoadImage", "inputs": {"image": "token_hot_tail_shot01.png"}},
    "6": {"class_type": "ImageScale", "inputs": {"image": ["4", 0], "upscale_method": "lanczos", "width": W, "height": H, "crop": "center"}},
    "7": {"class_type": "ImageScale", "inputs": {"image": ["5", 0], "upscale_method": "lanczos", "width": W, "height": H, "crop": "center"}},
    "8": {"class_type": "LoraLoaderModelOnly", "inputs": {"model": ["1", 0], "lora_name": "moss_image_edit_lora_bf16.safetensors", "strength_model": 1.0}},
    "9": {"class_type": "TextEncodeMOSSImageEdit", "inputs": {
        "clip": ["2", 0],
        "prompt": "anime illustration. Keep the exact same face and identity as Picture 1. "
                  "Only change her hair to short blue hair. Keep the white dress and the book. "
                  "Standing, white background.","vae": ["3", 0],
        "edit_format": "ai-toolkit",
        "images.image_0": ["6", 0], "images.image_1": ["7", 0],
        "image_strength": 1.0,
    }},
    "10": {"class_type": "TextEncodeMOSSImageEdit", "inputs": {
        "clip": ["2", 0],
        "prompt": "nsfw, worst quality, bad anatomy, watermark, jpeg artifact",
        "vae": ["3", 0],
        "edit_format": "ai-toolkit",
        "images.image_0": ["6", 0], "images.image_1": ["7", 0],
        "image_strength": 1.0,
    }},
    "11": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": W, "height": H, "batch_size": 1}},
    "12": {"class_type": "KSampler", "inputs": {
        "model": ["8", 0], "positive": ["9", 0], "negative": ["10", 0], "latent_image": ["11", 0],
        "seed": SEED, "steps": STEPS, "cfg": CFG,
        "sampler_name": "er_sde", "scheduler": "simple", "denoise": 1.0,
    }},
    "13": {"class_type": "VAEDecode", "inputs": {"samples": ["12", 0], "vae": ["3", 0]}},
    "14": {"class_type": "SaveImage", "inputs": {"images": ["13", 0], "filename_prefix": "transform_face"}},
}

req = urllib.request.Request(BASE + "/prompt",
                             data=json.dumps({"prompt": graph}).encode(),
                             headers={"Content-Type": "application/json"})
res = json.load(urllib.request.urlopen(req, timeout=30))
print("submit:", json.dumps(res, ensure_ascii=False)[:400])
if res.get("node_errors"):
    raise SystemExit("NODE_ERRORS " + json.dumps(res["node_errors"], ensure_ascii=False)[:500])
pid = res["prompt_id"]

t0 = time.time()
files = None
while time.time() - t0 < 240:
    time.sleep(2)
    h = json.load(urllib.request.urlopen(f"{BASE}/history/{pid}", timeout=10))
    if pid in h:
        st = h[pid]["status"]
        print("status:", st.get("status_str"), "elapsed:", round(time.time() - t0, 1), "s")
        if st.get("status_str") == "error":
            for m in st.get("messages", []):
                if m[0] == "execution_error":
                    print("ERR:", json.dumps(m[1], ensure_ascii=False)[:600])
            raise SystemExit(1)
        files = [i for o in h[pid]["outputs"].values() for i in o.get("images", [])]
        break

if not files:
    print("TIMEOUT")
    raise SystemExit(1)

img = files[0]
print("file:", img["filename"])
q = urllib.parse.urlencode({"filename": img["filename"], "subfolder": img.get("subfolder", ""), "type": img.get("type", "output")})
data = urllib.request.urlopen(f"{BASE}/view?{q}", timeout=60).read()
open(".tmp/transform_test.png", "wb").write(data)
print("saved .tmp/transform_test.png", len(data), "bytes")
