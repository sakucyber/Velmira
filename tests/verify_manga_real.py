"""Opt-in local GPU acceptance. Run reference stage, inspect it, then --render.

All images, workflows, scores and the database stay in ignored .tmp/.
No missing face/checker result counts as a pass. No silent seed selection.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", default=".tmp/m3-real")
    parser.add_argument("--render", action="store_true", help="Confirm the inspected reference and render ten fixed shots")
    parser.add_argument("--run-id", default="v1")
    parser.add_argument("--attempts", type=int, default=3, choices=[1, 2, 3])
    parser.add_argument("--reference-file", help="Upload an inspected reference from the same drawing style")
    args = parser.parse_args()
    root = Path(args.output).resolve(); root.mkdir(parents=True, exist_ok=True)
    os.environ["DATA_DIR"] = str(root)
    from fastapi.testclient import TestClient
    from server.app.main import app
    from manga_fixture import seed
    seed()
    base = "/api/books/m3-book/manga"
    with TestClient(app) as client:
        def call(path, payload=None, method="POST"):
            response = client.request(method, base + path, json=payload)
            response.raise_for_status()
            return response.json()

        def wait(job):
            deadline = time.monotonic() + 1500
            while job["status"] in {"queued", "running"} and time.monotonic() < deadline:
                time.sleep(2)
                job = call("/jobs/" + job["id"], method="GET")["job"]
            (root / (job["id"] + ".json")).write_text(json.dumps(job, ensure_ascii=False, indent=2), encoding="utf-8")
            print(json.dumps({k: job[k] for k in ("id", "status", "face_score", "attempts", "error")}), flush=True)
            return job

        capabilities = call("/probe")
        print(json.dumps({k: capabilities[k] for k in ("ok", "provider", "missing", "reference_mode")}), flush=True)
        if not args.render:
            job = wait(call("/jobs", {"kind": "reference", "form_id": "form_m3-char", "request_id": "m3-reference-v1"})["job"])
            print("Inspect reference: " + str(root / "assets/manga" / (job["history"][-1]["id"] + ".png")), flush=True)
            return
        if args.reference_file:
            response = client.post(base + "/forms/form_m3-char/refs", content=Path(args.reference_file).read_bytes())
            response.raise_for_status()
        refs = call("/forms", method="GET")["forms"][0]["refs"]
        if not refs:
            raise RuntimeError("Run the reference stage and inspect its image first")
        call("/refs/" + refs[0]["id"], {"accepted": True}, "PATCH")
        call("/config", {"max_attempts": args.attempts}, "PATCH")
        descriptions = [
            "facing camera, calm smile, holding a closed book",
            "looking slightly to the left, relaxed smile",
            "looking slightly to the right, attentive expression",
            "seated on a park bench, facing camera",
            "standing beside a tree, face clearly visible",
            "raising one hand in greeting, facing camera",
            "thoughtful expression, eye-level camera",
            "surprised expression, frontal face",
            "walking toward camera, smiling",
            "gentle smile, warm evening light",
        ]
        results = []
        for idx, action in enumerate(descriptions):
            job = wait(call("/jobs", {"scene_id": "m3-scene", "character_id": "m3-char", "shot": "portrait" if idx % 2 == 0 else "medium", "action": action, "seed": 424300 + idx * 1009, "request_id": "m3-panel-" + args.run_id + "-" + str(idx)})["job"])
            results.append({"id": job["id"], "status": job["status"], "score": job["face_score"], "seed": job["snapshot"]["seed"], "attempts": job["attempts"]})
        passed = sum(r["status"] == "done" and r["score"] is not None and r["score"] >= .55 for r in results)
        report = {"panels": results, "threshold": .55, "passed": passed, "total": 10, "accepted": passed >= 8, "provider": "comfyui", "reference": refs[0]["id"]}
        (root / ("acceptance-" + args.run_id + ".json")).write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps(report), flush=True)


if __name__ == "__main__":
    main()
