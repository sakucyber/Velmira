// Run against a disposable server; no GPU or face weights required.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8883';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-manga-'));
const sleep = ms => new Promise(r => setTimeout(r, ms));
const port = 9341, pending = new Map(), errors = [];
let browser, ws, sequence = 0;
async function api(url, method = 'GET', data) {
  const r = await fetch(base + url, { method, body: data && JSON.stringify(data), headers: data ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(r.ok, `${url}: ${r.status} ${await r.clone().text()}`); return r.json();
}
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!r.exceptionDetails, JSON.stringify(r.exceptionDetails)); return r.result.value;
}
async function waitFor(expression) {
  for (let i = 0; i < 150; i++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression);
}
async function clickText(text) {
  const point = await evaluate(`(() => { const b=[...document.querySelectorAll('.drawer.open button')].find(b=>b.textContent===${JSON.stringify(text)}); if(!b)throw new Error('Missing button'); b.scrollIntoView({block:'center'});const r=b.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 });
}
async function set(label, value) {
  await evaluate(`(() => {const n=document.querySelector('[aria-label=${JSON.stringify(label)}]'); n.value=${JSON.stringify(value)}; n.dispatchEvent(new Event('change',{bubbles:true}));})()`);
}
async function shot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const r = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(r.data, 'base64'));
}
try {
  const upload = await fetch(base + '/api/books?filename=M3-neutral.txt', { method: 'POST', body: '第一章 示例\n\n' + '这是一段中性测试正文。'.repeat(80), headers: { 'Content-Type': 'application/octet-stream' } });
  assert.ok(upload.ok); const book = (await upload.json()).book;
  // The neutral fixture has stable M2 data. Reuse its book when present.
  const books = (await api('/api/books')).books;
  const id = books.some(b => b.id === 'm3-book') ? 'm3-book' : book.id;
  const prefix = '/api/books/' + id + '/manga';
  const initialVersion = (await api(prefix + '/forms')).forms.find(f => f.form_key === 'base').version;
  await api(prefix + '/config', 'PATCH', { provider: 'mock' });
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(r => { ws.onopen = r; });
  ws.onmessage = e => { const m = JSON.parse(e.data); if (pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); } else if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + id + '&debug=1' });
  await waitFor('!!window.__velmira && !document.querySelector("#btn-ai").disabled');
  await evaluate('document.querySelector("#btn-ai").click()');
  await waitFor('!!document.querySelector(".drawer-ai.open")');
  await clickText('漫画生成'); await waitFor('!!document.querySelector(".manga-tabs") && document.querySelector(".manga-tabs").children.length===4');
  await clickText('出图设置'); await waitFor('!!document.querySelector("[aria-label=图像后端]")');
  await set('图像后端', 'mock'); await clickText('保存设置'); await sleep(300);
  await clickText('检测已保存的连接与模型');
  await waitFor('document.querySelector(".manga-studio").textContent.includes("占位图仅供流程验证")');
  await clickText('人物与参考图'); await waitFor('[...document.querySelectorAll(".drawer.open button")].some(b=>b.textContent==="生成参考图")'); await clickText('生成参考图');
  await waitFor('!!document.querySelector(".manga-ref img")');
  await clickText('确认参考图'); await sleep(300);
  assert.ok((await api(prefix + '/forms')).forms.some(f => f.refs.some(r => r.accepted)));
  await clickText('分镜'); await waitFor('!!document.querySelector("[aria-label=镜头]")');
  if (id === 'm3-book') { await set('场景', 'm3-scene'); await set('人物形态', 'form_m3-char'); }
  await set('动作与表情', 'smiling, facing camera'); await clickText('加入生成队列');
  await waitFor('[...document.querySelectorAll(".manga-job")].some(n=>n.querySelector("h4").textContent === "分镜 · 待复核")');
  await clickText('人工确认此图');
  await waitFor('[...document.querySelectorAll(".manga-job")].some(n=>n.textContent.includes("人工已确认"))');
  assert.ok((await api(prefix + '/jobs')).jobs.some(j => j.status === 'approved' && j.face_score === null));
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await shot('manga-desktop');
  console.log('[PASS] 设置检测、参考图人工闸门、真实队列、Mock 待复核与人工确认、桌面布局');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await clickText('出图设置'); await waitFor('!!document.querySelector("[aria-label=默认生图风格]")');
  assert.equal(await evaluate('document.querySelector("[aria-label=默认生图风格]").value'), 'anime');
  await set('默认生图风格', 'watercolor'); await clickText('保存设置');
  await waitFor('document.querySelector(".manga-studio [aria-label=画风]").value.includes("watercolor")');
  assert.equal((await api(prefix + '/config')).config.style_preset, 'watercolor');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  assert.equal(await evaluate('document.querySelector(".drawer-manga").getBoundingClientRect().right<=innerWidth'), true);
  await shot('manga-mobile-settings');
  await clickText('整书分析'); await waitFor('!!document.querySelector("progress[aria-label=整书阅读进度]")');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await shot('manga-mobile-book-analysis');
  await clickText('人物与参考图'); await waitFor('!!document.querySelector(".manga-editor")'); await shot('manga-mobile-refs');
  await evaluate('document.querySelector(".manga-editor").open=true');
  await set('发色', 'brown'); await clickText('保存并冻结');
  await waitFor('document.querySelector(".ai-identity").textContent.includes("brown hair")');
  const changed = (await api(prefix + '/forms')).forms.find(f => f.form_key === 'base');
  assert.equal(changed.version, initialVersion + 1); assert.ok(changed.refs.every(r => !r.accepted));
  console.log('[PASS] 面板编辑身份并升版，旧参考图撤销确认');
  await evaluate('document.querySelector(".drawer-close").click()');
  await waitFor('!document.querySelector(".drawer-manga.open")');
  await sleep(2800); assert.deepEqual(errors, []);
  console.log('[PASS] 390px 手机设置/参考图库无溢出，关闭抽屉释放轮询，零 JS 异常');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(300); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
}
