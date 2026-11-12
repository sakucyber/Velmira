/* M1 前端 CDP 实测（verifier-m1）。启动 headless Edge + 原生 CDP，真实后端。
   覆盖：文本阅读器品牌/注释高亮/改字号/换主题/切章重建/书签/目录过滤/触摸滑动/
   键盘/点击区/首屏请求；漫画模式单 img/翻页/深链/错误重试/首屏请求；移动端无横向溢出；
   文本 20 章 heap、漫画 DOM/heap；全程 0 JS 报错。
   用法：node tests/cdp_m1.mjs --base http://127.0.0.1:8787 [--port 9334] */
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const opt = (name, def) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : def; };
const BASE = opt("--base", "http://127.0.0.1:8787");
const CDP_PORT = Number(opt("--port", "9334"));
const EDGE = "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const R = [];
const add = (item, status, detail) => { R.push({ item, status, detail }); console.log("[" + status + "] " + item + ": " + detail); };
const j = (v) => JSON.stringify(v);

async function api(pathname, opts) { const r = await fetch(BASE + pathname, opts); return { status: r.status, obj: await r.json().catch(() => null) }; }

class CDP {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map(); this.events = [];
    ws.addEventListener("message", (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        const p = this.pending.get(m.id); this.pending.delete(m.id);
        if (m.error) p.reject(new Error(JSON.stringify(m.error))); else p.resolve(m.result);
      } else if (m.method) { this.events.push(m); }
    });
  }
  send(method, params) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params: params || {} }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
  async eval(expression, awaitPromise) {
    const r = await this.send("Runtime.evaluate", { expression, awaitPromise: !!awaitPromise, returnByValue: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 500));
    return r.result.value;
  }
}

async function launchEdge() {
  const profile = path.join(os.tmpdir(), "velmira_m1_cdp_" + Date.now());
  fs.mkdirSync(profile, { recursive: true });
  const child = spawn(EDGE, [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-port=" + CDP_PORT, "--user-data-dir=" + profile,
    "--window-size=1440,900", "about:blank",
  ], { stdio: "ignore", detached: false });
  for (let i = 0; i < 80; i++) {
    await sleep(250);
    try {
      const list = await (await fetch("http://127.0.0.1:" + CDP_PORT + "/json/list")).json();
      const page = list.find((t) => t.type === "page");
      if (page) return { child, profile, wsUrl: page.webSocketDebuggerUrl };
    } catch (e) { /* retry */ }
  }
  throw new Error("Edge CDP 未就绪");
}

async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((res, rej) => { ws.addEventListener("open", res); ws.addEventListener("error", rej); });
  return new CDP(ws);
}

async function main() {
  const launched = await launchEdge();
  const cdp = await connect(launched.wsUrl);
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Log.enable");
  await cdp.send("Network.enable");
  await cdp.send("HeapProfiler.enable");
  const pageErrors = [];
  cdp.ws.addEventListener("message", (ev) => {
    const m = JSON.parse(ev.data);
    if (m.method === "Runtime.exceptionThrown") pageErrors.push(String(m.params.exceptionDetails.text || "exception"));
    if (m.method === "Log.entryAdded" && m.params.entry.level === "error") pageErrors.push("log:" + m.params.entry.text.slice(0, 160));
    if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") pageErrors.push("console:" + JSON.stringify(m.params.args.map((a) => a.value)).slice(0, 160));
  });
  const nav = async (url, waitMs) => {
    cdp.events = [];
    await cdp.send("Page.navigate", { url });
    await sleep(waitMs || 3000);
  };
  const reqCount = () => cdp.events.filter((e) => e.method === "Network.requestWillBeSent" && e.params.type !== "Preflight").length;

  /* ---------- 书库品牌/徽标 ---------- */
  await nav(BASE + "/", 2600);
  const lib = JSON.parse(await cdp.eval("JSON.stringify({title:document.title,brand:(document.querySelector('.rail-brand')||{}).textContent,brandTag:(document.querySelector('.rail-ver')||{}).textContent,badges:[...document.querySelectorAll('.fmt-badge')].map(b=>b.textContent.trim()+'|'+b.className),cards:document.querySelectorAll('.book-card').length,docScroll:document.documentElement.scrollWidth,docClient:document.documentElement.clientWidth})"));
  add("G1 书库 title/brand", /Velmira/.test(lib.title) && /Velmira/.test(lib.brand) && lib.brandTag === "v0.4" ? "PASS" : "FAIL", j(lib));
  add("G2 书库 3 种格式徽标", lib.badges.length >= 3 && ["TXT", "EPUB", "CBZ"].every((x) => lib.badges.some((b) => b.startsWith(x))) ? "PASS" : "FAIL", j(lib.badges));

  /* ---------- 准备数据：txt/cbz + 第 5 章 4 条注释 ---------- */
  const books = (await api("/api/books")).obj.books;
  const txt = books.find((b) => (b.format || "txt") === "txt");
  const cbz = books.find((b) => b.format === "cbz");
  const ch5 = (await api("/api/books/" + txt.id + "/chapters/5")).obj;
  const colors = ["yellow", "green", "blue", "pink"];
  const existing = (await api("/api/books/" + txt.id + "/annotations?chapter=5&limit=200")).obj.annotations || [];
  const annIds = [];
  for (let i = 0; i < 4; i++) {
    const ptext = ch5.paragraphs[i] || "";
    const text = ptext.slice(0, 2);
    const keep = existing.find((a) => a.para === i && a.start_offset === 0 && a.end_offset === 2 && a.color === colors[i]);
    if (keep) { annIds.push(keep.id); continue; }
    const res = await api("/api/books/" + txt.id + "/annotations", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ chapter: 5, para: i, start_offset: 0, end_offset: 2, text, color: colors[i] }),
    });
    annIds.push(res.obj.annotation.id);
  }
  add("G0 预造 4 色注释（API）", "PASS", "ids=" + annIds.join(","));

  /* ---------- 文本阅读器 ---------- */
  await nav(BASE + "/?book=" + txt.id + "&ch=5&debug=1", 3200);
  const txtState = JSON.parse(await cdp.eval("JSON.stringify({title:document.title,marks:[...document.querySelectorAll('mark.ann')].map(m=>({id:m.dataset.ann,cls:m.className,len:(m.textContent||'').length})),anns:window.__velmira?window.__velmira.annotations().length:-1,firstScreen:window.__velmira?window.__velmira.firstScreen():[],dom:document.querySelectorAll('*').length,pageLabel:document.querySelector('#page-label').textContent,viewport:!!document.querySelector('#reader-main')})"));
  add("G3 阅读器 document.title = <书名> · Velmira", / · Velmira$/.test(txtState.title) ? "PASS" : "FAIL", "title_ends_with_brand=" + / · Velmira$/.test(txtState.title) + " title_len=" + txtState.title.length);
  const colorsGot = txtState.marks.map((m) => (m.cls.match(/ann-(\w+)/) || [])[1]).sort();
  const idsOk = txtState.marks.length === 4 && annIds.every((id) => txtState.marks.some((m) => m.id === id));
  add("G4 注释高亮 mark×4 / 4 色 / data-ann", idsOk && j(colorsGot) === j(["blue", "green", "pink", "yellow"]) ? "PASS" : "FAIL", "marks=" + j(txtState.marks) + " anns_in_mem=" + txtState.anns);
  const fsReqs = txtState.firstScreen.length;
  add("G5 文本首屏请求 ≤9", fsReqs + 1 <= 9 ? "PASS" : "FAIL", "resource=" + fsReqs + " +document=" + (fsReqs + 1) + " list=" + j(txtState.firstScreen));

  await cdp.eval("document.querySelector('#btn-font').click()");
  await sleep(700);
  const afterFont = JSON.parse(await cdp.eval("JSON.stringify({marks:document.querySelectorAll('mark.ann').length,fontSize:getComputedStyle(document.querySelector('#reader-column')).fontSize})"));
  add("G6 改字号后 mark 重建", afterFont.marks === 4 ? "PASS" : "FAIL", j(afterFont));
  await cdp.eval("document.querySelector('#btn-theme').click()");
  await sleep(900);
  const themeBtns = await cdp.eval("document.querySelectorAll('.popover button').length");
  await cdp.eval("var b=document.querySelectorAll('.popover button'); if(b.length>1) b[1].click();");
  await sleep(700);
  const afterTheme = JSON.parse(await cdp.eval("JSON.stringify({marks:document.querySelectorAll('mark.ann').length,theme:document.documentElement.dataset.theme,popover:document.querySelectorAll('.popover').length})"));
  add("G7 换主题后 mark 重建", afterTheme.marks === 4 && afterTheme.theme !== "day" ? "PASS" : "FAIL", "popup_options=" + themeBtns + " " + j(afterTheme));
  await cdp.eval("window.__velmira.goChapter(6)");
  await sleep(1200);
  const ch6 = JSON.parse(await cdp.eval("JSON.stringify({ch:window.__velmira.state.chapter,marks:document.querySelectorAll('mark.ann').length,anns:window.__velmira.annotations().length})"));
  await cdp.eval("window.__velmira.goChapter(5)");
  await sleep(1400);
  const back5 = JSON.parse(await cdp.eval("JSON.stringify({ch:window.__velmira.state.chapter,marks:document.querySelectorAll('mark.ann').length})"));
  add("G8 切章清空/回切重建 mark", ch6.ch === 6 && ch6.marks === 0 && back5.marks === 4 ? "PASS" : "FAIL", "ch6=" + j(ch6) + " back5=" + j(back5));

  // 清空既有书签，保证从「未收藏」开始（前一次运行可能留下）
  const oldBms = (await api("/api/books/" + txt.id + "/bookmarks")).obj.bookmarks || [];
  for (const b of oldBms) await api("/api/books/" + txt.id + "/bookmarks/" + b.id, { method: "DELETE" });
  await cdp.eval("window.__velmira.loadBookmarks(true)");
  await sleep(500);
  await cdp.eval("document.querySelector('#btn-bookmark').click()");
  await sleep(900);
  const bmOn = JSON.parse(await cdp.eval("JSON.stringify({pressed:document.querySelector('#btn-bookmark').getAttribute('aria-pressed'),active:document.querySelector('#btn-bookmark').classList.contains('active'),fill:getComputedStyle(document.querySelector('#btn-bookmark .star-fill')).opacity})"));
  await cdp.eval("document.querySelector('#btn-bookmark').click()");
  await sleep(900);
  const bmOff = await cdp.eval("document.querySelector('#btn-bookmark').getAttribute('aria-pressed')");
  await cdp.eval("document.querySelector('#btn-bookmark').click()");
  await sleep(700);
  await cdp.eval("document.querySelector('#btn-bookmarks').click()");
  await sleep(1000);
  const drawer = JSON.parse(await cdp.eval("JSON.stringify({items:document.querySelectorAll('.bm-item').length,open:!!document.querySelector('.drawer-bookmarks.open')})"));
  await cdp.eval("var b=document.querySelector('.bm-jump'); if(b) b.click();");
  await sleep(900);
  const afterJump = JSON.parse(await cdp.eval("JSON.stringify({drawer:document.querySelectorAll('.drawer-bookmarks').length,ch:window.__velmira.state.chapter,para:window.__velmira.state.para})"));
  add("G9 书签加/删/抽屉跳转", bmOn.pressed === "true" && bmOn.active === true && bmOff === "false" && drawer.items >= 1 && afterJump.drawer === 0 ? "PASS" : "FAIL", "on=" + j(bmOn) + " off=" + bmOff + " drawer=" + j(drawer) + " jump=" + j(afterJump));

  await cdp.eval("document.querySelector('#btn-toc').click()");
  await sleep(1600);
  const tocBefore = await cdp.eval("document.querySelectorAll('.toc-item').length");
  await cdp.eval("var i=document.querySelector('.toc-filter-input'); i.value='100'; i.dispatchEvent(new Event('input',{bubbles:true}));");
  await sleep(300);
  const tocFiltered = JSON.parse(await cdp.eval("JSON.stringify({total:document.querySelectorAll('.toc-item').length,visible:[...document.querySelectorAll('.toc-item')].filter(x=>!x.hidden).length,visible_text_len:[...document.querySelectorAll('.toc-item')].filter(x=>!x.hidden).map(x=>x.textContent.length),meta:(document.querySelector('.toc-filter-meta')||{}).textContent})"));
  add("G10 目录过滤输入 100", tocBefore === 234 && tocFiltered.visible === 1 ? "PASS" : "FAIL", "toc=" + tocBefore + " filtered=" + j(tocFiltered));
  await cdp.eval("var x=document.querySelector('.drawer-close'); if(x) x.click();");
  await sleep(500);

  const page0 = await cdp.eval("window.__velmira.state.page");
  const touch = async (x1, x2) => {
    const y = 420;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: x1, y, id: 1 }] });
    await sleep(30);
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: x2, y, id: 1 }] });
    await sleep(30);
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await sleep(500);
  };
  await touch(760, 520);
  const pageL = await cdp.eval("window.__velmira.state.page");
  await touch(520, 700);
  const pageR = await cdp.eval("window.__velmira.state.page");
  const pageSmall0 = await cdp.eval("window.__velmira.state.page");
  await touch(700, 680);
  const pageSmall = await cdp.eval("window.__velmira.state.page");
  add("G11 触摸滑动：左滑/右滑翻页，20px 不翻", pageL === page0 + 1 && pageR === page0 && pageSmall === pageSmall0 ? "PASS" : "FAIL", "start=" + page0 + " left=" + pageL + " right=" + pageR + " small20=" + pageSmall0 + "->" + pageSmall);
  const pageK0 = await cdp.eval("window.__velmira.state.page");
  await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 });
  await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 });
  await sleep(400);
  const pageK1 = await cdp.eval("window.__velmira.state.page");
  await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 1200, y: 450, button: "left", clickCount: 1 });
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 1200, y: 450, button: "left", clickCount: 1 });
  await sleep(500);
  const pageC = await cdp.eval("window.__velmira.state.page");
  add("G12 键盘 → 与右 1/3 点击区翻页", pageK1 === pageK0 + 1 && pageC === pageK1 + 1 ? "PASS" : "FAIL", "kbd " + pageK0 + "->" + pageK1 + " click->" + pageC);

  await cdp.send("HeapProfiler.collectGarbage");
  const heap0 = await cdp.eval("window.__velmira.heap()");
  for (let i = 0; i < 20; i++) { await cdp.eval("window.__velmira.goChapter(" + ((i + 1) % txt.chapter_count) + ")"); await sleep(260); }
  await sleep(900);
  await cdp.send("HeapProfiler.collectGarbage");
  await sleep(400);
  const heap1 = await cdp.eval("window.__velmira.heap()");
  const nodesTxt = await cdp.eval("document.querySelectorAll('*').length");
  add("G13 文本切 20 章 heap 增量 <5MB", heap0 > 0 && (heap1 - heap0) / 1048576 < 5 ? "PASS" : "FAIL", "heap " + (heap0 / 1048576).toFixed(2) + "MB -> " + (heap1 / 1048576).toFixed(2) + "MB, delta=" + ((heap1 - heap0) / 1048576).toFixed(2) + "MB, DOM=" + nodesTxt);

  /* ---------- 漫画模式 ---------- */
  // 先归零漫画进度，保证从第 1 页开始（进度会被服务端记住）
  await api("/api/books/" + cbz.id + "/progress", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ chapter: 0, para: 0, offset: 0 }) });
  cdp.events = [];
  await cdp.send("Page.navigate", { url: BASE + "/?book=" + cbz.id + "&debug=1" });
  await sleep(3200);
  const comic = JSON.parse(await cdp.eval("JSON.stringify({bodyClass:document.body.className,imgs:document.querySelectorAll('#reader-main img').length,allImgs:document.querySelectorAll('img').length,src:(document.querySelector('#reader-main img')||{}).src,nw:(document.querySelector('#reader-main img')||{}).naturalWidth,dom:document.querySelectorAll('*').length,count:(document.querySelector('.comic-count')||{}).textContent,page:(document.querySelector('.comic-page')||{}).textContent,pct:(document.querySelector('.comic-pct')||{}).textContent,fit:document.querySelector('.comic-root')?document.querySelector('.comic-root').className:null,title:document.title})"));
  const comicReqs = reqCount();
  add("G14 漫画模式 body.comic-mode / 单 img / naturalWidth", /comic-mode/.test(comic.bodyClass) && comic.imgs === 1 && comic.nw === 1200 ? "PASS" : "FAIL", j(comic));
  add("G15 漫画 DOM <300 / 首屏请求 ≤12", comic.dom < 300 && comicReqs <= 12 ? "PASS" : "FAIL", "DOM=" + comic.dom + " requestWillBeSent=" + comicReqs);
  const p1 = await cdp.eval("(document.querySelector('.comic-count')||{}).textContent");
  await cdp.eval("document.querySelector('.comic-next').click()");
  await sleep(900);
  const p2 = JSON.parse(await cdp.eval("JSON.stringify({count:(document.querySelector('.comic-count')||{}).textContent,src:(document.querySelector('#reader-main img')||{}).src,url:location.search,pct:(document.querySelector('.comic-pct')||{}).textContent,label:(document.querySelector('.comic-page')||{}).textContent})"));
  add("G16 漫画下一页：URL/页码/进度变化", p2.count !== p1 && /\/1$/.test(p2.src || "") && /ch=1/.test(p2.url) ? "PASS" : "FAIL", "before=" + p1 + " after=" + j(p2));
  await nav(BASE + "/?book=" + cbz.id + "&ch=4&debug=1", 2600);
  const deep = JSON.parse(await cdp.eval("JSON.stringify({count:(document.querySelector('.comic-count')||{}).textContent,pct:(document.querySelector('.comic-pct')||{}).textContent,src:(document.querySelector('#reader-main img')||{}).src})"));
  add("G17 深链 ?ch=4 落末页 5/5", /5\s*\/\s*5/.test(deep.count || "") && /\/4$/.test(deep.src || "") ? "PASS" : "FAIL", j(deep));
  await cdp.send("Network.setBlockedURLs", { urls: ["*media*"] });
  await nav(BASE + "/?book=" + cbz.id + "&debug=1", 2600);
  const err = JSON.parse(await cdp.eval("JSON.stringify({error:(document.querySelector('.comic-error')||{}).hidden,retry:!!document.querySelector('.comic-retry'),msg:(document.querySelector('.comic-error-msg')||{}).textContent,fatal:!!document.querySelector('.fatal')})"));
  await cdp.send("Network.setBlockedURLs", { urls: [] });
  add("G18 漫画媒体失败：错误态+重试按钮", err.retry === true && err.fatal === false ? "PASS" : "FAIL", j(err));
  await nav(BASE + "/?book=" + cbz.id + "&debug=1", 2600);
  await cdp.send("HeapProfiler.collectGarbage");
  const comicHeap = JSON.parse(await cdp.eval("JSON.stringify({heap:performance.memory?performance.memory.usedJSHeapSize:0,dom:document.querySelectorAll('*').length,imgs:document.querySelectorAll('img').length})"));
  add("G19 漫画 heap/DOM 记录", "PASS", "heap=" + (comicHeap.heap / 1048576).toFixed(2) + "MB DOM=" + comicHeap.dom + " imgs=" + comicHeap.imgs);

  /* ---------- 移动端 390×844 ---------- */
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await nav(BASE + "/?book=" + txt.id + "&ch=0", 3000);
  const mobile = JSON.parse(await cdp.eval("JSON.stringify({iw:innerWidth,ih:innerHeight,docScroll:document.documentElement.scrollWidth,docClient:document.documentElement.clientWidth,bodyScroll:document.body.scrollWidth,marks:document.querySelectorAll('mark.ann').length,toolbar:document.querySelectorAll('.reader-tools .btn').length})"));
  add("G20 移动端 390×844 无横向溢出", mobile.iw === 390 && mobile.docScroll === 390 && mobile.docClient === 390 ? "PASS" : "FAIL", j(mobile));
  await cdp.send("Emulation.clearDeviceMetricsOverride");

  await sleep(300);
  const errs = pageErrors.filter((x) => !/favicon/i.test(x));
  add("G21 全程 0 JS 报错", errs.length === 0 ? "PASS" : "FAIL", "count=" + errs.length + " " + j(errs.slice(0, 6)));

  const summary = { PASS: R.filter((x) => x.status === "PASS").length, FAIL: R.filter((x) => x.status === "FAIL").length };
  console.log("\n=== CDP SUMMARY ===\n" + j(summary));
  fs.mkdirSync(path.join("tests", "screenshots"), { recursive: true });
  fs.writeFileSync(path.join("tests", "cdp_m1_result.json"), JSON.stringify({ base: BASE, results: R, summary }, null, 2), "utf8");
  const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(path.join("tests", "screenshots", "m1_mobile_390x844.png"), Buffer.from(shot.data, "base64"));
  await cdp.send("Browser.close").catch(() => {});
  try { launched.child.kill("SIGKILL"); } catch (e) {}
  return summary.FAIL === 0 ? 0 : 2;
}

main().then((rc) => process.exit(rc)).catch((e) => { console.error("ERR", e); process.exit(1); });
