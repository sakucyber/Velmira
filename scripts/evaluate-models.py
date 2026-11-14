"""Bounded live evaluation using encrypted local profiles and original test fiction.

Run from the repository root: python scripts/evaluate-models.py
No imported books or credentials are written to the report.
"""
import concurrent.futures
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from server.app.ai import settings
from server.app.ai.providers import request_json
from server.app.manga.storyboard import SYSTEM, normalize

CASES = [
    {"id": "handoff", "text": "清晨，林澄走进温室。周遥已经站在木桌右侧。林澄站到桌子左侧，把手里的铜钥匙递给周遥。周遥接过钥匙，说：‘我去开门。’随后周遥走向温室的木门，林澄留在桌旁。",
     "characters": [{"id": "lin", "name": "林澄"}, {"id": "zhou", "name": "周遥"}], "previous_panels": [], "chapter_summary": "两人在温室交接铜钥匙，周遥随后去开门。"},
    {"id": "environment", "text": "夜色笼罩着空无一人的车站。站台上只有一张木椅。风吹过，长椅下的一张旧车票翻了个面。远处的信号灯由红变绿。",
     "characters": [], "previous_panels": [], "chapter_summary": "空车站中车票被风吹动，信号灯变色。"},
    {"id": "flashback", "text": "沈宁站在钟楼的窗边，右手握着一只银色怀表。她想起昨晚顾禾说过：‘钟声停了以后再打开。’那只是回忆，此刻顾禾并不在场。沈宁等到钟声停止，才用左手打开怀表。表盘里没有文字，只有一根断掉的指针。",
     "characters": [{"id": "shen", "name": "沈宁"}, {"id": "gu", "name": "顾禾"}], "previous_panels": [], "chapter_summary": "沈宁独自在钟楼等钟声停止后打开怀表，顾禾只出现在回忆里。"},
]
MODELS = [("local", "bonsai2-27b"), ("knd-text", "gpt-6-luna"), ("pie-text", "qwen3.8-27b"), ("pie-text", "gpt-oss-120b")]


def evaluate(item):
    connection, model = item
    cfg = settings.load_settings("profile-" + connection)
    output = []
    for case in CASES:
        start = time.monotonic()
        record = {"connection": connection, "model": model, "case": case["id"]}
        try:
            body = {
                "model": model, "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": json.dumps(case, ensure_ascii=False)}],
                "temperature": .2, "max_tokens": 6000}
            if cfg.get("json_mode", True): body["response_format"] = {"type": "json_object"}
            result = request_json(cfg["base_url"] + "/chat/completions", api_key=settings.api_key(cfg), body=body)
            content = result["choices"][0]["message"]["content"]
            record.update(output=content, usage=result.get("usage"), finish_reason=result["choices"][0].get("finish_reason"))
            panels = normalize(content, case)
            record.update(valid=True, panels=panels)
            # These are explicit narrow checks, not an automatic claim of overall accuracy.
            record["checks"] = {
                "quoted_evidence": all(p["source_quote"] in case["text"] for p in panels),
                "environment_has_no_cast": all(p["kind"] == "environment" for p in panels) if case["id"] == "environment" else None,
                "handoff_has_duo": any(p["kind"] == "duo" for p in panels) if case["id"] == "handoff" else None,
                "absent_character_not_drawn": not any(s["character_id"] == "gu" for p in panels for s in p["subjects"]) if case["id"] == "flashback" else None,
            }
        except Exception as exc:
            record.update(valid=False, error=str(exc).replace(settings.api_key(cfg), "[redacted]") if settings.api_key(cfg) else str(exc))
        record["seconds"] = round(time.monotonic() - start, 2)
        output.append(record)
        print(json.dumps({k: record[k] for k in ("model", "case", "valid", "seconds")}), flush=True)
    return output


if __name__ == "__main__":
    destination = Path(".tmp/model-eval")
    destination.mkdir(parents=True, exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        records = [r for group in pool.map(evaluate, MODELS) for r in group]
    (destination / "text-results.json").write_text(json.dumps(records, ensure_ascii=False, indent=2), encoding="utf-8")
    print("Saved .tmp/model-eval/text-results.json", flush=True)
