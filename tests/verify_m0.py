#!/usr/bin/env python
"""M0 验收 API 实测脚本（verifier 所有）。不修改 server/，只发请求 + 断言。

用法：python tests/verify_m0.py --base http://127.0.0.1:8780 [--json out.json]
需要服务已用独立 DATA_DIR 冷启动，且未导入任何书。
"""
from __future__ import annotations

import argparse
import http.client as httpclient
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
if TESTS_DIR not in sys.path:
    sys.path.insert(0, TESTS_DIR)
import fixture_book  # noqa: E402  tests/fixture_book.py

RESULTS = []
FIXTURE = fixture_book.parse_meta()
BOOK_PATH = FIXTURE["txt"]            # 运行时定位 book/ 第一本 txt（脱敏：不在 tests/ 硬编码书名）
BOOK_TITLE = FIXTURE["title"]
BOOK_AUTHOR = FIXTURE["author"]
BOOK_ID = None


def add(item, status, detail, evidence=""):
    RESULTS.append({"item": item, "status": status, "detail": detail, "evidence": evidence})
    print(f"[{status}] {item}: {detail}", flush=True)


def http(method, url, body=None, headers=None):
    req = urllib.request.Request(url, data=body, method=method)
    if headers:
        for k, v in headers.items():
            req.add_header(k, v)
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            raw = resp.read()
            status = resp.status
            ctype = resp.headers.get("Content-Type", "")
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        status = exc.code
        ctype = exc.headers.get("Content-Type", "")
    elapsed = (time.perf_counter() - started) * 1000
    try:
        obj = json.loads(raw.decode("utf-8"))
    except Exception:
        obj = None
    return status, ctype, obj, raw, elapsed


def api(method, path, body=None, headers=None, base=None):
    return http(method, (base or BASE) + path, body, headers)


def err_shape(obj):
    return (
        isinstance(obj, dict)
        and isinstance(obj.get("error"), dict)
        and isinstance(obj["error"].get("code"), str)
        and isinstance(obj["error"].get("message"), str)
    )


def main():
    global BASE, BOOK_ID
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8780")
    args = ap.parse_args()
    BASE = args.base.rstrip("/")

    # ---------------- 前置：冷启动健康检查
    st, ct, obj, raw, ms = api("GET", "/api/health")
    if st == 200 and obj and obj.get("version") == "0.5.1" and "mock" not in str(obj.get("version")):
        add("A0 冷启动健康检查", "PASS", f"GET /api/health -> 200 version=0.5.1 data_dir={obj.get('data_dir')}", f"{ms:.0f}ms")
    else:
        add("A0 冷启动健康检查", "FAIL", f"status={st} body={raw[:200]!r}")
        print(json.dumps(RESULTS, ensure_ascii=False, indent=1))
        return 1

    st, ct, obj, raw, ms = api("GET", "/api/books")
    cold_empty = isinstance(obj, dict) and obj.get("books") == []
    add("A0b 冷启动书库为空", "PASS" if cold_empty else "FAIL",
        f"GET /api/books -> {st} books={len(obj.get('books', [])) if isinstance(obj, dict) else 'N/A'}")

    # ---------------- A 导入
    book_path = BOOK_PATH
    data = open(book_path, "rb").read()
    bom = data[:3] == b"\xef\xbb\xbf"
    add("A 源文件前置", "PASS" if not bom else "FAIL",
        f"file={len(data)} bytes, BOM={'yes' if bom else 'no'}")
    filename = os.path.basename(book_path)
    q = urllib.parse.urlencode({"filename": filename, "force": "0"})
    st, ct, obj, raw, ms = api("POST", f"/api/books?{q}", data, {"Content-Type": "application/octet-stream"})
    if st != 201 or not isinstance(obj, dict):
        add("A 冷启动导入", "FAIL", f"status={st} body={raw[:300]!r}")
        print(json.dumps(RESULTS, ensure_ascii=False, indent=1))
        return 1
    book = obj.get("book") or {}
    BOOK_ID = book.get("id")
    checks = []
    checks.append(("HTTP 201", st == 201, st))
    checks.append(("chapter_count=234", book.get("chapter_count") == 234, book.get("chapter_count")))
    # 夹具的书头可能编辑过；按契约独立计算全文归一化长度，不依赖旧硬编码。
    normalized = re.sub("[\\u200b-\\u200f\\u2060\\ufeff]", "", data.decode("utf-8-sig").replace("\r\n", "\n").replace("\r", "\n"))
    checks.append(("total_chars=归一化源文件长度", book.get("total_chars") == len(normalized), book.get("total_chars")))
    checks.append(("paragraphs=15245", obj.get("paragraphs") == 15245, obj.get("paragraphs")))
    checks.append(("encoding=utf-8", book.get("encoding") == "utf-8", book.get("encoding")))
    checks.append(("import_ms<3000", obj.get("import_ms", 10**9) < 3000, obj.get("import_ms")))
    checks.append(("deduped=false", obj.get("deduped") is False, obj.get("deduped")))
    title_ok = book.get("title") == BOOK_TITLE
    author_ok = book.get("author") in (BOOK_AUTHOR, BOOK_AUTHOR + " ")
    checks.append(("title == 夹具解析标题", title_ok, title_ok))
    checks.append(("author == 夹具解析作者", author_ok, author_ok))
    bad = [c for c in checks if not c[1]]
    detail = " | ".join(f"{n}={v}" for n, ok, v in checks)
    add("A 冷启动导入断言", "PASS" if not bad else "FAIL",
        detail + ("" if not bad else " || 失败: " + ", ".join(c[0] for c in bad)),
        f"real_elapsed={ms:.0f}ms")
    add("A 导入响应字段全集", "PASS" if set(obj.keys()) >= {"book", "import_ms", "paragraphs", "deduped"} else "PARTIAL",
        f"keys={sorted(obj.keys())}")
    add("A book 字段全集", "PASS" if set(book.keys()) >= {"id", "title", "author", "format", "encoding", "total_chars", "chapter_count", "created_at"} else "PARTIAL",
        f"keys={sorted(book.keys())}")

    # ---------------- A TOC / 章节
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/toc")
    toc = obj.get("chapters", []) if isinstance(obj, dict) else []
    titles_nonempty = sum(1 for c in toc if (c.get("title") or "").strip())
    add("A TOC 234 项", "PASS" if st == 200 and len(toc) == 234 else "FAIL",
        f"status={st} items={len(toc)} first_idx={toc[0].get('idx') if toc else None} last_idx={toc[-1].get('idx') if toc else None} titles_nonempty={titles_nonempty}",
        f"{ms:.0f}ms")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/chapters/32")
    paras32 = obj.get("paragraphs", []) if isinstance(obj, dict) else []
    first32 = paras32[0] if paras32 else ""
    title32 = (obj or {}).get("title") or ""
    ok32 = st == 200 and isinstance(first32, str) and len(first32) > 5 and bool(title32.strip())
    add("A chapters/32 首段（结构性）", "PASS" if ok32 else "FAIL",
        f"status={st} title_nonempty={bool(title32.strip())} paragraphs={len(paras32)} first_len={len(first32)}",
        f"{ms:.0f}ms")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/chapters/0")
    joined = ""
    if isinstance(obj, dict):
        joined = (obj.get("title") or "") + "\n" + "\n".join(obj.get("paragraphs") or [])
    polluted = [k for k in ("书籍信息", "书名：", "【简介】") if k in joined]
    paras0_all = (obj.get("paragraphs") or []) if isinstance(obj, dict) else []
    title0 = (obj.get("title") or "") if isinstance(obj, dict) else ""
    add("A chapters/0 无书头污染", "PASS" if st == 200 and not polluted else "FAIL",
        f"status={st} paragraphs={len(paras0_all)} title_nonempty={bool(title0.strip())} polluted={polluted}",
        f"{ms:.0f}ms")
    first0 = paras0_all[0] if paras0_all else ""
    add("A chapters/0 首段结构", "PASS" if isinstance(first0, str) and len(first0) > 5 else "FAIL",
        f"paragraphs={len(paras0_all)} first_len={len(first0)}")
    text0 = "\n".join(p for p in paras0_all if isinstance(p, str))

    # ---------------- B 契约错误路径
    cases = [
        ("B1 chapters/234 -> 404 chapter_not_found", "GET", f"/api/books/{BOOK_ID}/chapters/234", None, 404,
         {"chapter_not_found"}),
        ("B2 未知书 -> 404 book_not_found", "GET", "/api/books/b_nonexistent", None, 404, {"book_not_found"}),
        ("B3 缺 filename -> 400 missing_filename", "POST", "/api/books", b"hello", 400, {"missing_filename"}),
        ("B4 空 body -> 400 empty_file", "POST", f"/api/books?filename=empty.txt", b"", 400, {"empty_file"}),
        ("B5 未知书 chapter -> 404", "GET", "/api/books/b_nonexistent/chapters/0", None, 404, {"book_not_found"}),
    ]
    hdrs = {"Content-Type": "application/octet-stream"}
    for name, method, path, body, want, codes in cases:
        st, ct, obj, raw, ms = api(method, path, body, hdrs if body is not None else None)
        ok = st == want and err_shape(obj) and obj["error"]["code"] in codes
        add(name, "PASS" if ok else "FAIL",
            f"status={st} code={(obj or {}).get('error', {}).get('code')} shape_ok={err_shape(obj)} msg={(obj or {}).get('error', {}).get('message', '')[:50]!r}",
            f"{ms:.0f}ms ctype={ct.split(';')[0]}")

    # B5b 65MB -> 413（http.client 手写 Content-Length，避免 urllib 内存拷贝）
    try:
        conn = httpclient.HTTPConnection(BASE.split("://")[1], timeout=120)
        size = 65 * 1024 * 1024
        conn.putrequest("POST", f"/api/books?filename=big.txt")
        conn.putheader("Content-Type", "application/octet-stream")
        conn.putheader("Content-Length", str(size))
        conn.endheaders()
        chunk = b"x" * (1024 * 1024)
        for _ in range(65):
            conn.send(chunk)
        resp = conn.getresponse()
        raw = resp.read()
        st = resp.status
        obj = json.loads(raw.decode("utf-8"))
        conn.close()
        ok = st == 413 and err_shape(obj) and obj["error"]["code"] == "file_too_large"
        add("B5 65MB -> 413 file_too_large", "PASS" if ok else "FAIL",
            f"status={st} code={(obj or {}).get('error', {}).get('code')} bytes={size}")
    except Exception as exc:
        add("B5 65MB -> 413 file_too_large", "FAIL", f"exception: {exc!r}")

    # B6 progress chapter 越界 -> 400 invalid_locator
    for ch in (234, 999):
        body = json.dumps({"chapter": ch, "para": 0, "offset": 0}).encode()
        st, ct, obj, raw, ms = api("PUT", f"/api/books/{BOOK_ID}/progress", body, {"Content-Type": "application/json"})
        ok = st == 400 and err_shape(obj) and obj["error"]["code"] == "invalid_locator"
        add(f"B6 progress chapter={ch} 越界 -> 400 invalid_locator", "PASS" if ok else "FAIL",
            f"status={st} code={(obj or {}).get('error', {}).get('code')}")

    # B7 非法 JSON body -> 400
    st, ct, obj, raw, ms = api("PUT", f"/api/books/{BOOK_ID}/progress", b"{not-json", {"Content-Type": "application/json"})
    add("B7 progress 非法 JSON -> 400", "PASS" if st == 400 and err_shape(obj) else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")

    # ---------------- C 进度
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/progress")
    p = (obj or {}).get("progress", {})
    add("C0 无记录默认进度", "PASS" if st == 200 and p.get("chapter") == 0 and p.get("percent") == 0.0 else "FAIL",
        f"status={st} progress={json.dumps(p, ensure_ascii=False)}")

    def put_progress(ch, para, off):
        b = json.dumps({"chapter": ch, "para": para, "offset": off, "percent": 0.5}).encode()
        return api("PUT", f"/api/books/{BOOK_ID}/progress", b, {"Content-Type": "application/json"})

    st, ct, obj, raw, ms = put_progress(233, 57, 0)
    p1 = (obj or {}).get("progress", {})
    add("C1 末章 para=57 offset=0 percent>=0.999", "PASS" if st == 200 and p1.get("percent", 0) >= 0.999 else "FAIL",
        f"status={st} percent={p1.get('percent')} chapter={p1.get('chapter')} para={p1.get('para')} offset={p1.get('offset')}")

    st, ct, obj, raw, ms = put_progress(233, 57, 46)
    p2 = (obj or {}).get("progress", {})
    add("C2 末章 offset=46 percent=1.0", "PASS" if st == 200 and abs(p2.get("percent", 0) - 1.0) < 1e-6 else "FAIL",
        f"status={st} percent={p2.get('percent')}")

    # C3 单调递增
    percents = []
    for ch in (0, 50, 100, 150, 200, 233):
        st, ct, obj, raw, ms = put_progress(ch, 0, 0)
        percents.append(((obj or {}).get("progress", {})).get("percent"))
    mono = all(isinstance(x, (int, float)) for x in percents) and all(
        percents[i] < percents[i + 1] for i in range(len(percents) - 1))
    add("C3 进度单调递增", "PASS" if mono else "FAIL", f"chapters 0,50,100,150,200,233 -> {percents}")

    # C4 GET 读取重算与 PUT 一致
    st_put, _, obj_put, _, _ = put_progress(32, 3, 7)
    st_get, _, obj_get, _, _ = api("GET", f"/api/books/{BOOK_ID}/progress")
    pput = (obj_put or {}).get("progress", {})
    pget = (obj_get or {}).get("progress", {})
    same = (st_put, st_get) == (200, 200) and all(
        pput.get(k) == pget.get(k) for k in ("chapter", "para", "offset", "percent"))
    add("C4 GET 重算与 PUT 一致", "PASS" if same else "FAIL",
        f"PUT={json.dumps(pput, ensure_ascii=False)} GET={json.dumps(pget, ensure_ascii=False)}")

    # C5 重复导入 dedupe
    st, ct, obj, raw, ms = api("POST", f"/api/books?{q}", data, {"Content-Type": "application/octet-stream"})
    ded = isinstance(obj, dict) and obj.get("deduped") is True and st == 200 and obj.get("book", {}).get("id") == BOOK_ID
    add("C5 重复导入 deduped=true", "PASS" if ded else "FAIL",
        f"status={st} deduped={(obj or {}).get('deduped')} id={(obj or {}).get('book', {}).get('id')} import_ms={(obj or {}).get('import_ms')}",
        f"{ms:.0f}ms")

    # ---------------- D 分段
    seg_body = json.dumps({
        "strategy": "paragraph_window",
        "params": {"target_chars": 900, "overlap_paras": 1, "min_chars": 400, "max_chars": 1500},
        "per_chapter": 1, "force": False,
    }).encode()
    st, ct, obj, raw, ms = api("POST", f"/api/books/{BOOK_ID}/segment", seg_body, {"Content-Type": "application/json"})
    seg1 = obj or {}
    checks = [
        ("segments=475", seg1.get("segments") == 475, seg1.get("segments")),
        ("selected=234", seg1.get("selected") == 234, seg1.get("selected")),
        ("picked_chars 214184±500", isinstance(seg1.get("picked_chars"), int) and abs(seg1["picked_chars"] - 214184) <= 500, seg1.get("picked_chars")),
        ("elapsed_ms<3000", seg1.get("elapsed_ms", 10**9) < 3000, seg1.get("elapsed_ms")),
        ("cached=false", seg1.get("cached") is False, seg1.get("cached")),
        ("params_hash 非空", bool(seg1.get("params_hash")), seg1.get("params_hash")),
    ]
    add("D1 分段统计标定", "PASS" if all(c[1] for c in checks) else "FAIL",
        " | ".join(f"{n}={v}" for n, ok, v in checks), f"real_elapsed={ms:.0f}ms")
    params_hash_1 = seg1.get("params_hash")

    st, ct, obj, raw, ms = api("POST", f"/api/books/{BOOK_ID}/segment", seg_body, {"Content-Type": "application/json"})
    seg2 = obj or {}
    add("D2 二次 POST cached=true", "PASS" if st == 200 and seg2.get("cached") is True and seg2.get("params_hash") == params_hash_1 else "FAIL",
        f"status={st} cached={seg2.get('cached')} params_hash={seg2.get('params_hash')} same_hash={seg2.get('params_hash') == params_hash_1} elapsed_ms={seg2.get('elapsed_ms')}")

    # D3 GET segments chapter=32 selected=1
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/segments?chapter=32&selected=1&limit=50")
    segs = obj.get("segments", []) if isinstance(obj, dict) else []
    top = segs[0] if segs else {}
    top_head = str(top.get("head") or "")
    top_title = str(top.get("title") or "")
    top_ok = (
        st == 200 and str(top.get("id")) == "c33-s0" and top.get("chapter") == 32
        and abs((top.get("raw") or 0) - 3.414) <= 0.01
        and abs((top.get("score") or 0) - 0.999) <= 0.002
        and len(top_head) > 0 and len(top_title) > 0
    )
    add("D3 GET segments Top1 = c33-s0（结构性）", "PASS" if top_ok else "FAIL",
        f"status={st} count={len(segs)} top_id={top.get('id')} chapter={top.get('chapter')} idx={top.get('idx')} raw={top.get('raw')} score={top.get('score')} head_len={len(top_head)} title_len={len(top_title)}",
        f"{ms:.0f}ms")
    text_leak = [k for k in top.keys() if k == "text"]
    add("D4 GET segments 不含 text 全文字段", "PASS" if not text_leak else "FAIL",
        f"top_keys={sorted(top.keys())}")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/segments")
    segs_default = obj.get("segments", []) if isinstance(obj, dict) else []
    add("D5 GET segments 默认 limit=50", "PASS" if st == 200 and len(segs_default) == 50 else "FAIL",
        f"status={st} returned={len(segs_default)}")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/segments?limit=2000")
    segs_big = obj.get("segments", []) if isinstance(obj, dict) else []
    add("D6 GET segments limit=2000 被钳制(不报错)", "PASS" if st == 200 and len(segs_big) <= 1000 else "FAIL",
        f"status={st} returned={len(segs_big)} (实现上限 _MAX_LIMIT=1000; 全书仅 475 段故无法观测 1000 边界)",
        f"{ms:.0f}ms")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/segments?limit=-5")
    segs_neg = obj.get("segments", []) if isinstance(obj, dict) else []
    add("D7 GET segments limit 负数钳制到 1", "PASS" if st == 200 and len(segs_neg) == 1 else "FAIL",
        f"status={st} returned={len(segs_neg)}")

    # ---------------- E 检索（搜索词运行时从正文截取，不硬编码内容）
    SEARCH2 = fixture_book.pick_term(text0, 2)
    SEARCH3 = fixture_book.pick_term(text0, 3)
    SEARCH1 = fixture_book.pick_term(text0, 1)
    qs = urllib.parse.urlencode({"q": SEARCH2, "limit": 20})
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/search?{qs}")
    cold = obj or {}
    hits = cold.get("hits", [])
    cold_ms = cold.get("elapsed_ms")
    idx = cold.get("index", {})
    snippet_ok = bool(hits) and all(("<b>" + SEARCH2 + "</b>") in (h.get("snippet") or "") for h in hits)
    add("E1 搜索 2 字动态词（冷，含建索引）", "PASS" if st == 200 and hits and idx.get("chapters_indexed") == 234 and idx.get("degraded") is False else "FAIL",
        f"status={st} elapsed_ms(resp)={cold_ms} wall={ms:.0f}ms hits={len(hits)} index={json.dumps(idx)} first=ch{hits[0].get('chapter') if hits else None}/p{hits[0].get('para') if hits else None}",
        f"{ms:.0f}ms")
    add("E2 snippet 含 <b>2 字动态词</b> 且已转义", "PASS" if snippet_ok else "FAIL",
        f"hits={len(hits)} all_bold={snippet_ok}")

    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/search?{qs}")
    warm = obj or {}
    warm_hits = warm.get("hits", [])
    warm_resp_ms = warm.get("elapsed_ms")
    add("E3 搜索 2 字动态词（温）<200ms", "PASS" if st == 200 and warm_resp_ms is not None and warm_resp_ms < 200 else "FAIL",
        f"status={st} elapsed_ms(resp)={warm_resp_ms} wall={ms:.0f}ms hits={len(warm_hits)} (上限 200ms)")
    add("E4 hits <= limit", "PASS" if len(warm_hits) <= 20 else "FAIL", f"hits={len(warm_hits)} limit=20")

    # E5 注入 <script>
    inj = "<script>alert(1)</script>"
    qs_inj = urllib.parse.urlencode({"q": inj})
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/search?{qs_inj}")
    body_text = raw.decode("utf-8", "replace")
    hit_blobs = json.dumps((obj or {}).get("hits", []), ensure_ascii=False)
    raw_hits = "<script>" in hit_blobs
    q_echo_count = body_text.count("<script>")
    add("E5 注入 q 的 snippet 无未转义 <script>", "PASS" if st == 200 and not raw_hits else "FAIL",
        f"status={st} hits={len((obj or {}).get('hits', []))} raw_script_in_hits={raw_hits} raw_script_in_body={q_echo_count} (body 中仅可能来自回显 q 字段)",
        f"{ms:.0f}ms")

    # E6 单字 degraded（单字同样从正文动态截取）
    qs1 = urllib.parse.urlencode({"q": SEARCH1 or (SEARCH2[:1])})
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/search?{qs1}")
    mi = obj or {}
    add("E6 q=单字（动态）degraded=true 不报错", "PASS" if st == 200 and mi.get("index", {}).get("degraded") is True else "FAIL",
        f"status={st} degraded={mi.get('index', {}).get('degraded')} hits={len(mi.get('hits', []))} elapsed_ms={mi.get('elapsed_ms')}",
        f"{ms:.0f}ms")

    # E7 空 q -> 400
    st, ct, obj, raw, ms = api("GET", f"/api/books/{BOOK_ID}/search?q=")
    add("E7 q 空 -> 400 empty_query", "PASS" if st == 400 and err_shape(obj) and (obj or {}).get("error", {}).get("code") == "empty_query" else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")

    # E8 2 字走 LIKE（按 chapter/para 排序），>=3 字走 bm25
    qs2 = urllib.parse.urlencode({"q": SEARCH2})
    st2, _, obj2, _, _ = api("GET", f"/api/books/{BOOK_ID}/search?{qs2}")
    hits2 = (obj2 or {}).get("hits", [])
    order2 = [(h["chapter"], h["para"]) for h in hits2]
    sorted2 = order2 == sorted(order2)
    qs3 = urllib.parse.urlencode({"q": SEARCH3})
    st3, _, obj3, _, _ = api("GET", f"/api/books/{BOOK_ID}/search?{qs3}")
    hits3 = (obj3 or {}).get("hits", [])
    scores3 = [h.get("score") for h in hits3]
    add("E8 2 字动态词 LIKE 按 chapter/para 升序", "PASS" if st2 == 200 and sorted2 else "FAIL", f"order={order2[:5]} sorted={sorted2}")
    add("E9 >=3 字动态词走 bm25 排序（负分升序）", "PASS" if st3 == 200 and hits3 and scores3 == sorted(scores3) else "FAIL",
        f"status={st3} hits={len(hits3)} scores={scores3[:8]}")

    out = {"base": BASE, "book_id": BOOK_ID, "results": RESULTS,
           "summary": {s: sum(1 for r in RESULTS if r["status"] == s) for s in ("PASS", "FAIL", "PARTIAL")}}
    print("\n=== SUMMARY ===")
    print(json.dumps(out["summary"], ensure_ascii=False))
    with open(os.path.join("tests", "verify_m0_result.json"), "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=2)
    return 0 if out["summary"]["FAIL"] == 0 else 2


if __name__ == "__main__":
    sys.exit(main())
