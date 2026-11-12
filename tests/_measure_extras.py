
import json, urllib.request, statistics, time
BASE = "http://127.0.0.1:8780"
def get(path):
    with urllib.request.urlopen(BASE + path, timeout=120) as r:
        return json.loads(r.read().decode("utf-8"))
book_id = "b_e12b55"
toc = get(f"/api/books/{book_id}/toc")["chapters"]
chars = [c["chars"] for c in toc]
print("toc_items=", len(toc))
print("sum_chapter_chars=", sum(chars), " max=", max(chars), " min=", min(chars))
segs = get(f"/api/books/{book_id}/segments?limit=2000")["segments"]
raws = sorted(s["raw"] for s in segs)
chars_seg = [s["chars"] for s in segs]
picked = [s for s in segs if s["selected"] == 1]
print("segments_total=", len(segs), " selected=", len(picked))
print("picked_chars=", sum(s["chars"] for s in picked))
print("raw_min=", raws[0], " raw_max=", raws[-1], " raw_p50=", statistics.median(raws))
print("seg_chars_mean=", round(sum(chars_seg)/len(chars_seg), 1))
print("score_min=", min(s["score"] for s in segs), " score_max=", max(s["score"] for s in segs))
# 温检索复测 3 次
for i in range(3):
    t0 = time.perf_counter()
    r = get(f"/api/books/{book_id}/search?q=%E6%98%9F%E7%91%95&limit=20")
    print("search_warm_%d resp_ms=%s wall_ms=%.1f hits=%d degraded=%s" % (i, r["elapsed_ms"], (time.perf_counter()-t0)*1000, len(r["hits"]), r["index"]["degraded"]))
