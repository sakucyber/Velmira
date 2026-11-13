// Disposable backend only. Uses a local fake text API and the existing mock image provider.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8886';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-auto-'));
const port = 9343, pending = new Map(), errors = [], materials = [];
const sleep = ms => new Promise(r => setTimeout(r, ms));
let browser, ws, sequence = 0, savedSettings;
const textAPI = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    const system = input.messages[0].content, material = JSON.parse(input.messages[1].content);
    let output;
    if (system.includes('整书阅读分析员')) output = {
      synopsis: '林澄沿小路走入花园，随后在长椅旁停下。', summary: '林澄来到花园', kind: 'scene', location: 'garden', time_of_day: 'day', entities: ['花园'],
      characters: [{ name: '林澄', card: { hair_color: 'black' }, evidence: [{ field: 'hair_color', value: 'black', quote: '黑色短发' }] }],
    };
    else if (system.includes('连载漫画编剧')) {
      materials.push(material);
      output = { panels: [
        { character_id: material.characters[0].id, shot: 'medium', action: 'walk into the garden in daylight', caption: '林澄沿小路走进花园。', dialogue: '', speaker: '' },
        { character_id: material.characters[0].id, shot: 'reaction', action: 'stop beside a bench after walking', caption: '随后在长椅旁停下。', dialogue: '', speaker: '' },
      ] };
    } else if (system.includes('上下文总结编辑')) output = { summary: '林澄走进花园后在长椅旁停下，动作承接上一格。', location: 'garden', moment: 'stop', visible_action: 'stop beside a bench' };
    else if (system.includes('single comic-panel')) output = { visual_prompt: 'A single adult with black short hair stands beside a bench in a quiet garden in daylight, continuing the previous walk, face clearly visible.' };
    else throw new Error('Unexpected text stage');
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify(output) } }] }));
  } catch (err) { response.writeHead(500); response.end('Fixture output failed'); }
});
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
  for (let i = 0; i < 300; i++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression);
}
async function click(selector) {
  await waitFor(`!!document.querySelector(${JSON.stringify(selector)}) && !document.querySelector(${JSON.stringify(selector)}).disabled`);
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
}
async function shot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const r = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(r.data, 'base64'));
}
try {
  await new Promise(resolve => textAPI.listen(0, '127.0.0.1', resolve));
  savedSettings = (await api('/api/ai/settings')).settings;
  await api('/api/ai/settings', 'PATCH', { enabled: true, auto_analyze: false, base_url: `http://127.0.0.1:${textAPI.address().port}/v1`, model: 'gpt-oss-120b' });
  const text = '第一章 花园\n\n林澄留着黑色短发，沿小路走进花园。随后，林澄在长椅旁停下。\n\n第二章 小路\n\n林澄留着黑色短发，又沿小路走进花园。随后，林澄在长椅旁停下。';
  const upload = await fetch(base + '/api/books?filename=自动漫画示例.txt', { method: 'POST', body: text, headers: { 'Content-Type': 'application/octet-stream' } });
  assert.ok(upload.ok); const book = (await upload.json()).book;
  const prefix = '/api/books/' + book.id + '/manga';
  await api(prefix + '/config', 'PATCH', { provider: 'mock', width: 256, height: 256 });
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(r => { ws.onopen = r; });
  ws.onmessage = e => { const m = JSON.parse(e.data); if (pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); } else if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + book.id + '&ch=1&debug=1' });
  await waitFor('!!window.__velmira && !!document.querySelector(".auto-manga-toggle") && !document.querySelector(".auto-manga-toggle").disabled');
  await shot('automatic-reader-desktop');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await click('.auto-manga-toggle');
  await waitFor('!!document.querySelector(".drawer-automatic.open [role=switch]")');
  assert.equal((await api(prefix + '/auto')).auto.start_chapter, 1);
  await click('.drawer-automatic [role=switch]');
  await waitFor('document.querySelector(".drawer-automatic [role=switch]").getAttribute("aria-checked")==="false"');
  const paused = (await api(prefix + '/auto')).auto;
  assert.equal(paused.status, 'paused');
  await click('.drawer-automatic [role=switch]');
  const resumed = (await api(prefix + '/auto')).auto;
  assert.equal(resumed.run_id, paused.run_id);
  for (let i = 0; i < 120; i++) {
    const state = (await api(prefix + '/auto')).auto;
    assert.notEqual(state.status, 'error', JSON.stringify(state));
    if (state.status === 'done') break;
    await sleep(500);
  }
  assert.equal((await api(prefix + '/auto')).auto.status, 'done');
  await waitFor('!!document.querySelector(".automatic-page img") && document.querySelector(".automatic-page img").complete && document.querySelector(".automatic-page img").naturalWidth>0');
  await shot('automatic-draft-desktop');
  const pages = (await api(prefix + '/auto/pages')).pages;
  assert.ok(pages.length > 0); assert.ok(pages.every(p => p.chapter === 1 && p.status === 'draft' && p.pending_review > 0));
  assert.ok(materials.length > 0);
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await waitFor('document.documentElement.scrollWidth<=innerWidth');
  await shot('automatic-draft-mobile');
  await click('.drawer-automatic .drawer-close');
  await waitFor('!document.querySelector(".drawer-automatic.open")');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  assert.ok(await evaluate('document.querySelector(".auto-manga-toggle").getBoundingClientRect().width>=80'));
  await shot('automatic-reader-mobile');
  await send('Page.navigate', { url: base + '/?book=' + book.id + '&ch=1&debug=1' });
  await waitFor('!!window.__velmira && !!document.querySelector(".auto-manga-view")');
  await click('.auto-manga-view');
  await waitFor('!!document.querySelector(".automatic-page img")');
  assert.deepEqual(errors, []);
  console.log('[PASS] 阅读页开关、当前章起点、暂停续跑、真实整书分析与出图队列、草稿拼页、刷新持久化、390px 布局与零 JS 异常');
  console.log('Preview: ' + base + '/?book=' + book.id + '&ch=1');
} finally {
  if (savedSettings) {
    const { enabled, auto_analyze, base_url, model } = savedSettings;
    await api('/api/ai/settings', 'PATCH', { enabled, auto_analyze, ...(base_url ? { base_url } : {}), ...(model ? { model } : {}) });
  }
  textAPI.closeAllConnections(); await new Promise(resolve => textAPI.close(resolve));
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(300); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
}
