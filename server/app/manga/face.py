import json
import os
from pathlib import Path
import subprocess
import sys

from .. import config


def evaluate_face(reference, image, settings=None):
    settings = settings or {}
    python = settings.get("face_python") or os.environ.get("VELMIRA_FACE_PYTHON", "" if getattr(sys, "frozen", False) else sys.executable)
    models = settings.get("face_models") or os.environ.get("VELMIRA_FACE_MODELS", str(config.DATA_DIR / "models" / "buffalo_l"))
    if not python:
        return {"status": "unavailable", "reason": "请配置 VELMIRA_FACE_PYTHON 和 VELMIRA_FACE_MODELS"}
    try:
        source = Path(__file__).with_name("face_worker.py").read_text(encoding="utf-8")
        result = subprocess.run([python, "-c", source, json.dumps({"models": models, "reference": str(reference), "image": str(image)})], capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120,
                                creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        return json.loads(result.stdout.strip().splitlines()[-1])
    except (OSError, ValueError, IndexError, subprocess.TimeoutExpired) as exc:
        return {"status": "unavailable", "reason": str(exc)}
