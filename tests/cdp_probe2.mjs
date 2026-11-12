
const CDP_PORT = 9333;
const BASE = "http://127.0.0.1:8780";
const BOOK = "b_e12b55";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function targets() { const res = await fetch("http://127.0.0.1:" + CDP_PORT + "/json/list"); return res.json(); }
class CDP {
  constructor(ws) { this.ws = ws; this.id = 0; this.pending = new Map();
    ws.addEventListener("message", (ev) => { const m = JSON.parse(ev.data); if (m.id && this.pending.has(m.id)) { const p = this.pending.get(m.id); this.pending.delete(m.id); if (m.error) p.reject(new Error(JSON.stringify(m.error))); else p.resolve(m.result); } }); }
  send(method, params) { const id = ++this.id; this.ws.send(JSON.stringify({ id: id, method: method, params: params || {} })); return new Promise((resolve, reject) => this.pending.set(id, { resolve: resolve, reject: reject })); }
  async eval(expression, awaitPromise) { const r = await this.send("Runtime.evaluate", { expression: expression, awaitPromise: !!awaitPromise, returnByValue: true }); if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 400)); return r.result.value; }
}
async function connect() {
  for (let i = 0; i < 40; i++) { try { const list = await targets(); const page = list.find((t) => t.type === "page"); if (page) { const ws = new WebSocket(page.webSocketDebuggerUrl); await new Promise((res, rej) => { ws.addEventListener("open", res); ws.addEventListener("error", rej); }); return new CDP(ws); } } catch (e) {} await sleep(250); }
  throw new Error("no cdp");
}
const M = "(function(){var d=document.documentElement;var v=document.querySelector(\"#reader-viewport\");var c=document.querySelector(\"#reader-column\");var tb=document.querySelector(\".topbar\");var vr=v?v.getBoundingClientRect():null;var cr=c?c.getBoundingClientRect():null;var tr=tb?tb.getBoundingClientRect():null;return JSON.stringify({iw:innerWidth,ih:innerHeight,docScroll:d.scrollWidth,docClient:d.clientWidth,vpScroll:v?v.scrollWidth:null,vpClient:v?v.clientWidth:null,vpRectW:vr?Math.round(vr.width):null,vpRectRight:vr?Math.round(vr.right):null,colScrollW:c?c.scrollWidth:null,colClientW:c?c.clientWidth:null,colRectW:cr?Math.round(cr.width):null,colRight:cr?Math.round(cr.right):null,topbarW:tr?Math.round(tr.width):null,topbarScroll:tb?tb.scrollWidth:null,columnCount:c?getComputedStyle(c).columnCount:null,mediaMatch:matchMedia(\"(max-width: 600px)\").matches});})()";
async function main() {
  const cdp = await connect();
  await cdp.send("Page.enable"); await cdp.send("Runtime.enable");
  const out = {};
  await cdp.send("Page.navigate", { url: BASE + "/?book=" + BOOK });
  await sleep(4000);
  out.desktop = JSON.parse(await cdp.eval(M));
  // 390 mobile emulation (like Chrome DevTools device mode)
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await sleep(1800);
  out.mobile_true = JSON.parse(await cdp.eval(M));
  const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
  const fs = await import("node:fs");
  fs.writeFileSync("tests/screenshots/mobile_cdp_390x844.png", Buffer.from(shot.data, "base64"));
  // mobile false
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await sleep(1500);
  out.mobile_false = JSON.parse(await cdp.eval(M));
  const shot2 = await cdp.send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync("tests/screenshots/mobile_cdp_390x844_nomobile.png", Buffer.from(shot2.data, "base64"));
  // 1440 desktop
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  await sleep(1500);
  out.desktop_1440 = JSON.parse(await cdp.eval(M));
  const shot3 = await cdp.send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync("tests/screenshots/desktop_cdp_1440x900.png", Buffer.from(shot3.data, "base64"));
  console.log(JSON.stringify(out, null, 1));
}
main().then(() => process.exit(0)).catch((e) => { console.error("ERR", e); process.exit(1); });
