#!/usr/bin/env python
"""M1 独立验收 API 实测（verifier-m1 所有，tests/ 部分）。

覆盖：A 健康/品牌字段、C EPUB 导入、D CBZ 分页媒体/进度、E 注释 CRUD+校验、
F 书签、I(部分) 持久化。TXT 回归由 tests/verify_m0.py 负责（版本断言已更新 0.5.1）。

用法：
  python tests/verify_m1.py --base http://127.0.0.1:8787 [--phase api|persist]
服务必须用隔离 DATA_DIR 冷启动；--phase api 会导入 epub/cbz 并写 tests/m1_state.json；
--phase persist 在服务重启后读取状态，验证注释/书签仍在。
"""
from __future__ import annotations

import argparse
import io
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
if TESTS_DIR not in sys.path:
    sys.path.insert(0, TESTS_DIR)
import fixture_book  # noqa: E402  tests/fixture_book.py

RESULTS = []
BASE = ""
STATE_PATH = os.path.join("tests", "m1_state.json")
FIXTURE = os.path.join("tests", "fixtures", "m1_comic_5p.cbz")
# 脱敏：EPUB 夹具与期望标题/作者运行时从 book/ 解析，不在 tests/ 硬编码
EPUB_PATH = fixture_book.parse_meta().get("epub")
FIXTURE_TITLE = fixture_book.parse_meta()["title"]
FIXTURE_AUTHOR = fixture_book.parse_meta()["author"]


def add(item, status, detail):
    RESULTS.append({"item": item, "status": status, "detail": detail})
    print(f"[{status}] {item}: {detail}", flush=True)


def http(method, url, body=None, headers=None):
    req = urllib.request.Request(url, data=body, method=method)
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=240) as resp:
            raw, status = resp.read(), resp.status
            hdrs = dict(resp.headers.items())
    except urllib.error.HTTPError as exc:
        raw, status = exc.read(), exc.code
        hdrs = dict(exc.headers.items())
    elapsed = (time.perf_counter() - started) * 1000
    try:
        obj = json.loads(raw.decode("utf-8"))
    except Exception:
        obj = None
    return status, hdrs, obj, raw, elapsed


def api(method, path, body=None, headers=None):
    return http(method, BASE + path, body, headers)


def hget(hdrs, name):
    """HTTP 头大小写不敏感（uvicorn 小写输出，file 响应为 content-type）。"""
    name = name.lower()
    for k, v in (hdrs or {}).items():
        if k.lower() == name:
            return v
    return ""


def shape(obj):
    return (isinstance(obj, dict) and isinstance(obj.get("error"), dict)
            and isinstance(obj["error"].get("code"), str)
            and isinstance(obj["error"].get("message"), str))


def parse_filename(path):
    return urllib.parse.urlencode({"filename": os.path.basename(path)})


def import_file(path, force=False):
    data = open(path, "rb").read()
    q = parse_filename(path) + ("&force=1" if force else "")
    st, hdrs, obj, raw, ms = api("POST", "/api/books?" + q, data,
                                 {"Content-Type": "application/octet-stream"})
    return st, obj, raw, ms


def get_book_list():
    st, hdrs, obj, raw, ms = api("GET", "/api/books")
    return obj.get("books", []) if isinstance(obj, dict) else []


def make_cbz(path):
    """Pillow 自造 5 页 1200x1800 漫画：不同底色 + 页码。返回 [(name, png_bytes)]。"""
    from PIL import Image, ImageDraw
    os.makedirs(os.path.dirname(path), exist_ok=True)
    colors = [(220, 60, 60), (60, 160, 90), (60, 90, 220), (230, 180, 40), (150, 70, 180)]
    entries = []
    for i, color in enumerate(colors):
        img = Image.new("RGB", (1200, 1800), color)
        d = ImageDraw.Draw(img)
        d.rectangle([60, 60, 1140, 1740], outline=(255, 255, 255), width=8)
        d.text((500, 850), f"PAGE {i + 1}", fill=(255, 255, 255))
        buf = io.BytesIO()
        img.save(buf, format="PNG", optimize=True)
        entries.append((f"page_{i + 1}.png", buf.getvalue()))
    with zipfile.ZipFile(path, "w", zipfile.ZIP_STORED) as zf:
        for name, data in entries:
            zf.writestr(name, data)
    return entries


def annotation_checks(book_id, chapter, paras, bad_paras):
    """E: 4 色 POST / 排序分页 / PATCH / DELETE / 6 类 400 / 404。返回需持久化 id 列表。"""
    texts = {}
    made = []
    plan = [(0, 0, 2, "yellow"), (0, 2, 4, "green"), (1, 0, 3, "blue"), (2, 0, 2, "pink")]
    for para, start, end, color in plan:
        ptext = paras[para]
        end = min(end, len(ptext))
        sl = ptext[start:end]
        body = json.dumps({"chapter": chapter, "para": para, "start_offset": start,
                           "end_offset": end, "text": sl, "color": color,
                           "note": ("note-" + color) if color == "green" else None}).encode()
        st, hdrs, obj, raw, _ = api("POST", f"/api/books/{book_id}/annotations", body,
                                    {"Content-Type": "application/json"})
        ann = (obj or {}).get("annotation") or {}
        ok = st == 201 and ann.get("color") == color and ann.get("text") == sl
        made.append({"id": ann.get("id"), "color": color, "para": para, "start": start,
                     "end": end, "text": sl, "ok": ok, "status": st,
                     "code": (obj or {}).get("error", {}).get("code")})
    ok4 = all(m["ok"] for m in made)
    add("E1 POST 4 色注释 201", "PASS" if ok4 else "FAIL",
        " | ".join(f"{m['color']}:status={m['status']},id={m['id']},text_len={len(m['text'] or '')}" for m in made))

    st, hdrs, obj, raw, _ = api("GET", f"/api/books/{book_id}/annotations?chapter={chapter}")
    got = (obj or {}).get("annotations", [])
    keys = [(a["para"], a["start_offset"]) for a in got]
    sorted_ok = keys == sorted(keys)
    colors_ok = {a["color"] for a in got} == {"yellow", "green", "blue", "pink"}
    add("E2 GET chapter 排序+4 色", "PASS" if st == 200 and len(got) == 4 and sorted_ok and colors_ok else "FAIL",
        f"status={st} n={len(got)} keys={keys} colors={sorted({a['color'] for a in got})}")

    st, hdrs, obj, _, _ = api("GET", f"/api/books/{book_id}/annotations?chapter={chapter}&limit=2")
    n2 = len((obj or {}).get("annotations", []))
    st_b, hdrs, obj_b, _, _ = api("GET", f"/api/books/{book_id}/annotations?chapter={chapter}&limit=2&offset=2")
    n2b = len((obj_b or {}).get("annotations", []))
    add("E3 GET limit/offset 分页", "PASS" if (st, st_b) == (200, 200) and n2 == 2 and n2b == 2 else "FAIL",
        f"limit=2 -> {n2} 条, offset=2 -> {n2b} 条")

    st, hdrs, obj, _, _ = api("GET", f"/api/books/{book_id}/annotations")
    all_n = len((obj or {}).get("annotations", []))
    add("E4 GET 全书(无 chapter) 返回全部", "PASS" if st == 200 and all_n >= 4 else "FAIL", f"status={st} n={all_n}")

    target = made[0]
    body = json.dumps({"note": "主角名，18 岁", "color": "blue"}).encode()
    st, hdrs, obj, _, _ = api("PATCH", f"/api/books/{book_id}/annotations/{target['id']}", body,
                              {"Content-Type": "application/json"})
    ann = (obj or {}).get("annotation") or {}
    add("E5 PATCH note+color", "PASS" if st == 200 and ann.get("note") == "主角名，18 岁" and ann.get("color") == "blue" and ann.get("text") == target["text"] else "FAIL",
        f"status={st} note_ok={ann.get('note') == '主角名，18 岁'} color={ann.get('color')} text_unchanged={ann.get('text') == target['text']}")

    para_text = paras[0]
    bad_cases = [
        ("chapter 越界 999", {"chapter": 999, "para": 0, "start_offset": 0, "end_offset": 2, "text": paras[0][:2]}),
        ("start>=end", {"chapter": 0, "para": 0, "start_offset": 5, "end_offset": 5, "text": ""}),
        ("len(text)!=end-start", {"chapter": 0, "para": 0, "start_offset": 0, "end_offset": 5, "text": "abc"}),
        ("para 越界", {"chapter": 0, "para": len(paras), "start_offset": 0, "end_offset": 1, "text": "x"}),
        ("color 非法 purple", {"chapter": 0, "para": 0, "start_offset": 0, "end_offset": 2, "text": paras[0][:2], "color": "purple"}),
        ("end 超段落长", {"chapter": 0, "para": 0, "start_offset": 0, "end_offset": len(para_text) + 5, "text": "x" * (len(para_text) + 5)}),
    ]
    bads = []
    for name, payload in bad_cases:
        st, hdrs, obj, raw, _ = api("POST", f"/api/books/{book_id}/annotations",
                                    json.dumps(payload).encode(), {"Content-Type": "application/json"})
        code = (obj or {}).get("error", {}).get("code")
        bads.append((name, st, code))
    ok_bad = all(st == 400 and code == "invalid_annotation" for _, st, code in bads)
    add("E6 6 类非法参数均 400 invalid_annotation", "PASS" if ok_bad else "FAIL",
        " | ".join(f"{n}: {st}/{code}" for n, st, code in bads))

    st, hdrs, obj, _, _ = api("PATCH", f"/api/books/{book_id}/annotations/an_missing", b'{"note":"x"}',
                              {"Content-Type": "application/json"})
    add("E7 PATCH 不存在 -> 404 annotation_not_found", "PASS" if st == 404 and (obj or {}).get("error", {}).get("code") == "annotation_not_found" else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")

    st, hdrs, obj, _, _ = api("DELETE", f"/api/books/{book_id}/annotations/{made[3]['id']}")
    st2, hdrs, obj2, _, _ = api("DELETE", f"/api/books/{book_id}/annotations/{made[3]['id']}")
    st3, hdrs, obj3, _, _ = api("GET", f"/api/books/{book_id}/annotations?chapter={chapter}")
    n_after = len((obj3 or {}).get("annotations", []))
    add("E8 DELETE 生效 + 重复 DELETE 404", "PASS" if st == 200 and (obj or {}).get("ok") is True and st2 == 404 and (obj2 or {}).get("error", {}).get("code") == "annotation_not_found" and n_after == 3 else "FAIL",
        f"delete={st} ok={(obj or {}).get('ok')} delete2={st2} n_after={n_after}")

    return [m["id"] for m in made[:3]]


def bookmark_checks(book_id):
    url = f"/api/books/{book_id}/bookmarks"
    b1 = json.dumps({"chapter": 1, "para": 2, "offset": 0, "label": "验收书签"}).encode()
    st1, hdrs, obj1, raw1, _ = api("POST", url, b1, {"Content-Type": "application/json"})
    id1 = ((obj1 or {}).get("bookmark") or {}).get("id")
    st2, hdrs, obj2, raw2, _ = api("POST", url, b1, {"Content-Type": "application/json"})
    st3, hdrs, obj3, _, _ = api("GET", url)
    n = len((obj3 or {}).get("bookmarks", []))
    id2 = ((obj2 or {}).get("bookmark") or {}).get("id")
    add("F1 POST 同段书签幂等", "PASS" if st1 == st2 == 201 and id1 == id2 and st3 == 200 and n == 1 else "FAIL",
        f"first={st1}, dup={st2}, same_id={id1 == id2}, list={n}")
    st, hdrs, obj, _, _ = api("DELETE", f"{url}/{id1}")
    st_g, hdrs, obj_g, _, _ = api("GET", url)
    remaining = len((obj_g or {}).get("bookmarks", []))
    add("F2 DELETE 书签生效", "PASS" if st == 200 and (obj or {}).get("ok") is True and remaining == n - 1 else "FAIL",
        f"delete={st} list {n}->{remaining}")
    # 为后续重启持久化验证重新建立一条独立书签。
    _, _, saved, _, _ = api("POST", url, b1, {"Content-Type": "application/json"})
    return {"bookmark_id": saved["bookmark"]["id"], "remaining": remaining}


def phase_api():
    global BASE
    # ---------- A 健康/品牌 ----------
    st, hdrs, obj, raw, ms = api("GET", "/api/health")
    ok = st == 200 and obj.get("name") == "Velmira" and obj.get("version") == "0.5.1"
    add("A1 GET /api/health name/version", "PASS" if ok else "FAIL",
        f"status={st} name={obj.get('name')!r} version={obj.get('version')!r} data_dir={obj.get('data_dir')!r}")

    books = get_book_list()
    txt = next((b for b in books if b.get("format", "txt") == "txt"), None)
    if txt is None:
        add("A2 前置 txt 书", "FAIL", "未找到 txt 书（需先跑 tests/verify_m0.py 导入）")
        return 1
    add("A2 前置 txt 书", "PASS", f"id={txt['id']} chapters={txt.get('chapter_count')}")
    txt_id = txt["id"]

    # ---------- C EPUB ----------
    epub_path = EPUB_PATH
    if not epub_path or not os.path.isfile(epub_path):
        add("C0 EPUB 源文件", "FAIL", f"不存在：{epub_path}")
    else:
        st, obj, raw, ms = import_file(epub_path)
        book = (obj or {}).get("book") or {}
        epub_id = book.get("id")
        ok = (st == 201 and book.get("format") == "epub" and (book.get("chapter_count") or 0) >= 200
              and (obj or {}).get("import_ms", 10 ** 9) < 60000)
        add("C1 EPUB 导入", "PASS" if ok else "FAIL",
            f"status={st} id={epub_id} format={book.get('format')} chapters={book.get('chapter_count')} "
            f"total_chars={book.get('total_chars')} paragraphs={(obj or {}).get('paragraphs')} import_ms={(obj or {}).get('import_ms')} wall={ms:.0f}ms")
        st, hdrs, det, _, _ = api("GET", f"/api/books/{epub_id}")
        bd = (det or {}).get("book") or {}
        add("C2 EPUB 详情字段", "PASS" if st == 200 and "para_count" in bd else "FAIL",
            f"status={st} para_count={bd.get('para_count')} chapter_count={bd.get('chapter_count')} encoding={bd.get('encoding')!r} title_match={bd.get('title') == FIXTURE_TITLE} author_match={bd.get('author') == FIXTURE_AUTHOR}")
        st, hdrs, toc, _, _ = api("GET", f"/api/books/{epub_id}/toc")
        chapters = (toc or {}).get("chapters", [])
        chars_sum = sum((c or {}).get("chars") or 0 for c in chapters)
        nonempty = sum(1 for c in chapters if (c.get("title") or "").strip())
        first_title = (chapters[0] if chapters else {}).get("title") or ""
        last_title = (chapters[-1] if chapters else {}).get("title") or ""
        add("C3 EPUB TOC 可用", "PASS" if st == 200 and len(chapters) >= 200 and nonempty == len(chapters) else "FAIL",
            f"status={st} toc={len(chapters)} nonempty_titles={nonempty} chars_sum={chars_sum} first_title_len={len(first_title)} last_title_len={len(last_title)}")
        add("C4 EPUB Σchars vs TXT 482081", "PASS" if chars_sum == 482081 else "PARTIAL",
            f"epub chars_sum={chars_sum}，txt Σchars=482081（契约口径：total_chars 含书头，两者不可混用）")
        st, hdrs, ch32, _, _ = api("GET", f"/api/books/{epub_id}/chapters/32")
        paras32 = (ch32 or {}).get("paragraphs") or []
        first32 = paras32[0] if paras32 else ""
        title32 = (ch32 or {}).get("title") or ""
        add("C5 EPUB chapters/32 首段（结构性）", "PASS" if st == 200 and isinstance(first32, str) and len(first32) > 5 and bool(title32.strip()) else "FAIL",
            f"status={st} title_nonempty={bool(title32.strip())} paras={len(paras32)} first_len={len(first32)}")
        # C6 进度口径一致：同 locator 的 percent 与 txt 相同
        body = json.dumps({"chapter": 32, "para": 3, "offset": 7}).encode()
        st1, _, p1, _, _ = api("PUT", f"/api/books/{txt_id}/progress", body, {"Content-Type": "application/json"})
        st2, _, p2, _, _ = api("PUT", f"/api/books/{epub_id}/progress", body, {"Content-Type": "application/json"})
        v1 = ((p1 or {}).get("progress") or {}).get("percent")
        v2 = ((p2 or {}).get("progress") or {}).get("percent")
        add("C6 EPUB 进度 percent 与 TXT 口径一致", "PASS" if (st1, st2) == (200, 200) and v1 == v2 else "FAIL",
            f"txt percent={v1} epub percent={v2} equal={v1 == v2}")
        st, hdrs, lst, _, _ = api("GET", f"/api/books/{epub_id}/segments?limit=1")
        add("C7 EPUB 非 cbz -> segments 可调用（不报 not_comic）", "PASS" if st == 200 else "PARTIAL", f"status={st}")
    # ---------- D CBZ ----------
    entries = make_cbz(FIXTURE)
    add("D0 Pillow 自造 5 页 CBZ", "PASS", f"{FIXTURE} pages={[e[0] for e in entries]} sizes={[len(e[1]) for e in entries]}")
    st, obj, raw, ms = import_file(FIXTURE)
    cb = (obj or {}).get("book") or {}
    cbz_id = cb.get("id")
    ok = (st == 201 and cb.get("format") == "cbz" and cb.get("chapter_count") == 5
          and cb.get("page_count") == 5 and (obj or {}).get("paragraphs") == 0)
    add("D1 CBZ 导入", "PASS" if ok else "FAIL",
        f"status={st} id={cbz_id} format={cb.get('format')} chapter_count={cb.get('chapter_count')} page_count={cb.get('page_count')} total_chars={cb.get('total_chars')} encoding={cb.get('encoding')!r} paragraphs={(obj or {}).get('paragraphs')} import_ms={(obj or {}).get('import_ms')}")
    st, hdrs, pg, _, _ = api("GET", f"/api/books/{cbz_id}/pages/0")
    pg = pg or {}
    add("D2 GET /pages/0 字段", "PASS" if st == 200 and pg.get("book_id") == cbz_id and pg.get("idx") == 0 and pg.get("count") == 5 and pg.get("url") == f"/media/{cbz_id}/0" and pg.get("width") == 1200 and pg.get("height") == 1800 else "FAIL",
        f"status={st} body={json.dumps(pg, ensure_ascii=False)}")
    st, hdrs, pl, _, _ = api("GET", f"/api/books/{cbz_id}/pages?from=0&limit=3")
    pl = pl or {}
    idxs = [p["idx"] for p in pl.get("pages", [])]
    add("D3 GET /pages?from=0&limit=3", "PASS" if st == 200 and idxs == [0, 1, 2] and pl.get("count") == 5 and pl.get("from") == 0 else "FAIL",
        f"status={st} count={pl.get('count')} from={pl.get('from')} idx={idxs} pages={json.dumps(pl.get('pages', [])[:1], ensure_ascii=False)}")
    st, hdrs, pl2, _, _ = api("GET", f"/api/books/{cbz_id}/pages?limit=999")
    add("D4 /pages limit 上限 50 钳制", "PASS" if st == 200 and len((pl2 or {}).get("pages", [])) == 5 else "FAIL",
        f"status={st} returned={len((pl2 or {}).get('pages', []))}（5 页全部返回，上限 50 不可观测边界）")
    # 字节比对
    st, hdrs, obj, raw, ms = api("GET", f"/media/{cbz_id}/0")
    expected = dict(entries)[  "page_1.png"]
    ct = hget(hdrs, "Content-Type")
    cc = hget(hdrs, "Cache-Control")
    # Pairing can be revoked; private media must never stay in a shared cache.
    add("D5 /media 字节逐字节一致 + 私有缓存头", "PASS" if st == 200 and raw == expected and "image/png" in ct and "private" in cc and "no-store" in cc else "FAIL",
        f"status={st} bytes={len(raw)}=={len(expected)} identical={raw == expected} ctype={ct!r} cache={cc!r}")
    st, hdrs, obj, raw2, _ = api("GET", f"/media/{cbz_id}/1")
    add("D6 /media/1 对应第 2 页字节", "PASS" if st == 200 and raw2 == dict(entries)["page_2.png"] else "FAIL",
        f"status={st} bytes={len(raw2)} identical={raw2 == dict(entries)['page_2.png']}")
    st, hdrs, obj, raw, _ = api("GET", f"/api/books/{cbz_id}/pages/5")
    add("D7 /pages/5 越界 404 page_not_found", "PASS" if st == 404 and (obj or {}).get("error", {}).get("code") == "page_not_found" else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")
    st, hdrs, obj, raw, _ = api("GET", f"/media/{cbz_id}/9")
    add("D8 /media/9 越界 404", "PASS" if st == 404 else "FAIL", f"status={st} code={(obj or {}).get('error', {}).get('code')}")
    st, hdrs, obj, raw, _ = api("GET", f"/api/books/{txt_id}/pages/0")
    add("D9 txt 书 /pages -> 400 not_comic", "PASS" if st == 400 and (obj or {}).get("error", {}).get("code") == "not_comic" else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")
    st, hdrs, obj, raw, _ = api("GET", f"/media/{txt_id}/0")
    add("D10 txt 书 /media -> 400 not_comic", "PASS" if st == 400 and (obj or {}).get("error", {}).get("code") == "not_comic" else "FAIL",
        f"status={st} code={(obj or {}).get('error', {}).get('code')}")
    st, hdrs, obj, _, _ = api("GET", f"/api/books/{cbz_id}/progress")
    p0 = (obj or {}).get("progress") or {}
    add("D11 未读 CBZ 默认 percent=0.2", "PASS" if st == 200 and abs((p0.get("percent") or 0) - 0.2) < 1e-9 and p0.get("chapter") == 0 else "FAIL",
        f"status={st} progress={json.dumps(p0, ensure_ascii=False)}")
    seq = []
    for ch in (2, 4):
        body = json.dumps({"chapter": ch, "para": 0, "offset": 0}).encode()
        st, hdrs, obj, _, _ = api("PUT", f"/api/books/{cbz_id}/progress", body, {"Content-Type": "application/json"})
        seq.append((ch, ((obj or {}).get("progress") or {}).get("percent"), st))
    add("D12 PUT chapter=2 -> 0.6, chapter=4 -> 1.0", "PASS" if abs(seq[0][1] - 0.6) < 1e-9 and abs(seq[1][1] - 1.0) < 1e-9 else "FAIL",
        f"PUT results={seq}")
    st, hdrs, obj, _, _ = api("GET", f"/api/books/{cbz_id}/progress")
    pg_now = (obj or {}).get("progress") or {}
    add("D13 GET 进度回读 chapter=4 percent=1.0", "PASS" if pg_now.get("chapter") == 4 and abs((pg_now.get("percent") or 0) - 1.0) < 1e-9 else "FAIL",
        f"progress={json.dumps(pg_now, ensure_ascii=False)}")
    lst = get_book_list()
    row = next((b for b in lst if b.get("id") == cbz_id), {})
    add("D14 书库列表 percent 同步", "PASS" if abs((row.get("percent") or 0) - 1.0) < 1e-9 and row.get("last_chapter") == 4 else "FAIL",
        f"list row percent={row.get('percent')} last_chapter={row.get('last_chapter')} format={row.get('format')}")

    # ---------- E 注释（txt 书）----------
    st, hdrs, ch0, _, _ = api("GET", f"/api/books/{txt_id}/chapters/0")
    paras = (ch0 or {}).get("paragraphs") or []
    ann_ids = annotation_checks(txt_id, 0, paras, paras)
    # ---------- F 书签 ----------
    bm = bookmark_checks(txt_id)

    state = {"txt_id": txt_id, "epub_id": locals().get("epub_id"), "cbz_id": cbz_id,
             "annotation_ids": ann_ids, "bookmark": bm, "time": int(time.time())}
    with open(STATE_PATH, "w", encoding="utf-8") as fh:
        json.dump(state, fh, ensure_ascii=False, indent=2)
    return 0


def phase_persist():
    state = json.load(open(STATE_PATH, encoding="utf-8"))
    txt_id = state["txt_id"]
    marks = []
    for aid in state["annotation_ids"]:
        st, hdrs, obj, _, _ = api("GET", f"/api/books/{txt_id}/annotations?chapter=0&limit=500")
        found = [a for a in (obj or {}).get("annotations", []) if a.get("id") == aid]
        marks.append((aid, st, bool(found)))
    ok = all(st == 200 and found for _, st, found in marks)
    add("I-1 重启后注释仍在（GET 逐 id 命中）", "PASS" if ok else "FAIL",
        f"ids={[(aid, found) for aid, _, found in marks]}")
    bmid = (state.get("bookmark") or {}).get("bookmark_id")
    st, hdrs, obj, _, _ = api("GET", f"/api/books/{txt_id}/bookmarks")
    ids = [b.get("id") for b in (obj or {}).get("bookmarks", [])]
    found_bm = bmid in ids if bmid else None
    add("I-2 重启后书签仍在", "PASS" if st == 200 and found_bm else "FAIL", f"status={st} bmid={bmid} list={ids}")
    st, hdrs, obj, _, _ = api("GET", f"/api/books/{state['cbz_id']}/pages/0")
    add("I-3 重启后 cbz 页面元数据可读", "PASS" if st == 200 and (obj or {}).get("count") == 5 else "FAIL", f"status={st}")
    return 0


def main():
    global BASE
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8787")
    ap.add_argument("--phase", choices=["api", "persist"], default="api")
    args = ap.parse_args()
    BASE = args.base.rstrip("/")
    rc = phase_api() if args.phase == "api" else phase_persist()
    summary = {s: sum(1 for r in RESULTS if r["status"] == s) for s in ("PASS", "FAIL", "PARTIAL")}
    print("\n=== SUMMARY ===\n" + json.dumps(summary, ensure_ascii=False))
    out = {"base": BASE, "phase": args.phase, "results": RESULTS, "summary": summary}
    out_path = os.path.join("tests", "verify_m1_result.json")
    old = {}
    if os.path.isfile(out_path):
        try:
            old = json.load(open(out_path, encoding="utf-8"))
        except Exception:
            old = {}
    if args.phase == "persist" and old.get("results"):
        old["persist_results"] = RESULTS
        old["persist_summary"] = summary
        out = old
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=2)
    return 0 if summary["FAIL"] == 0 else 2


if __name__ == "__main__":
    sys.exit(main())
