
const CDP_PORT = 9333;
const BASE = "http://127.0.0.1:8780";
const BOOK = "b_e12b55";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function targets() {
  const res = await fetch("http://127.0.0.1:" + CDP_PORT + "/json/list");
  return res.json();
}

class CDP {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map(); this.events = [];
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const p = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) p.reject(new Error(JSON.stringify(msg.error))); else p.resolve(msg.result);
      } else if (msg.method) this.events.push(msg);
    });
  }
  send(method, params) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id: id, method: method, params: params || {} }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve: resolve, reject: reject }));
  }
  async eval(expression, awaitPromise) {
    const r = await this.send("Runtime.evaluate", { expression: expression, awaitPromise: !!awaitPromise, returnByValue: true, timeout: 120000 });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 500));
    return r.result.value;
  }
}

async function connect() {
  for (let i = 0; i < 80; i++) {
    try {
      const list = await targets();
      const page = list.find((t) => t.type === "page");
      if (page) {
        const ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((res, rej) => { ws.addEventListener("open", res); ws.addEventListener("error", rej); });
        return new CDP(ws);
      }
    } catch (e) { /* retry */ }
    await sleep(250);
  }
  throw new Error("CDP connect failed");
}

const METRICS = "(function(){var d=document.documentElement;var b=document.body;var col=document.querySelector(\"#reader-column\");var ch=document.querySelector(\"#chapter-label\");var pl=document.querySelector(\"#page-label\");var db=document.querySelector(\"#debug-badge\");return JSON.stringify({url:location.href,innerWidth:innerWidth,innerHeight:innerHeight,scrollWidth:d.scrollWidth,clientWidth:d.clientWidth,bodyScrollWidth:b?b.scrollWidth:null,horizontalOverflow:d.scrollWidth>d.clientWidth+1,nodes:document.querySelectorAll(\"*\").length,heapMB:performance.memory?Math.round(performance.memory.usedJSHeapSize/104857.6)/10:null,chapterLabel:ch?ch.textContent:null,pageLabel:pl?pl.textContent:null,debugBadge:db?db.textContent:null,resources:performance.getEntriesByType(\"resource\").map(function(e){return e.name.replace(location.origin,\"\");}),hasArp:!!window.__arp,paragraphs:document.querySelectorAll(\"#reader-column p\").length,textLen:col&&col.textContent?col.textContent.length:0});})()";

const LOOP = "(async function(){var log=[];var before={heapMB:Math.round(__arp.heap()/104857.6)/10,nodes:__arp.nodes(),baseline:Math.round(__arp.baselineHeap()/104857.6)/10};for(var i=1;i<=20;i++){await __arp.loadChapter(i,{para:0});await new Promise(function(r){setTimeout(r,60);});log.push({ch:i,heapMB:Math.round(__arp.heap()/104857.6)/10,nodes:__arp.nodes()});}return JSON.stringify({before:before,log:log,switches:__arp.switches().length});})()";

async function main() {
  const cdp = await connect();
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  const out = {};

  await cdp.send("Page.navigate", { url: BASE + "/?book=" + BOOK + "&debug=1" });
  await sleep(4500);
  out.desktop_first = JSON.parse(await cdp.eval(METRICS));

  await cdp.eval("document.querySelector(\"#reader-viewport\").dispatchEvent(new MouseEvent(\"click\",{clientX:innerWidth*0.9,clientY:300,bubbles:true}))");
  await sleep(900);
  out.after_page_turn = JSON.parse(await cdp.eval(METRICS));

  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await sleep(1800);
  out.mobile_390x844 = JSON.parse(await cdp.eval(METRICS));
  await cdp.send("Emulation.clearDeviceMetricsOverride");
  await sleep(600);

  out.chapter_stress = JSON.parse(await cdp.eval(LOOP, true));

  out.before_reload = JSON.parse(await cdp.eval(METRICS));
  await cdp.send("Page.navigate", { url: BASE + "/?book=" + BOOK + "&debug=1" });
  await sleep(4000);
  out.after_reload = JSON.parse(await cdp.eval(METRICS));

  await cdp.eval("document.querySelector(\"#btn-toc\").click()");
  await sleep(1500);
  out.after_toc_click = JSON.parse(await cdp.eval(METRICS));
  out.toc_items = await cdp.eval("document.querySelectorAll(\"#toc-list li, #toc-list [data-ch], .toc-item\").length");
  out.drawer_visible = await cdp.eval("!!document.querySelector(\"#drawer-layer\") && !document.querySelector(\"#drawer-layer\").hidden");

  await cdp.eval("document.querySelector(\"#btn-search\").click()");
  await sleep(800);
  out.after_search_open = JSON.parse(await cdp.eval(METRICS));

  console.log(JSON.stringify(out, null, 1));
}
main().then(() => process.exit(0)).catch((e) => { console.error("ERR", e); process.exit(1); });
