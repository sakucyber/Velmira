// Verify the one-shot text-free page in the real manga drawer.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8895';
const port = Number(process.env.CDP_PORT || 9352);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-single-'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0;
const pending = new Map(), errors = [];
const browserPath = process.env.EDGE_PATH || ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe'].find(candidate => fs.existsSync(candidate));
assert.ok(browserPath, 'Chrome or Edge is required');
const plan = { title: '无字测试页', panels: Array.from({ length: 4 }, (_, index) => ({
  shot: index % 2 ? 'reaction' : 'wide', characters: [], action: 'A lone figure walks through a quiet street, step ' + (index + 1),
  environment: 'empty street at dusk', lighting: 'soft dusk light', emotion: 'calm' })) };
const textAPI = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    if (!String(input.messages?.[0]?.content || '').includes('漫画整页导演')) throw new Error('unexpected stage');
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify(plan) } }] }));
  } catch { response.writeHead(500); response.end('fixture failed'); }
});

async function api(url, method = 'GET', data) {
  const response = await fetch(base + url, { method, body: data && JSON.stringify(data), headers: data ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(response.ok, method + ' ' + url + ': ' + response.status + ' ' + await response.clone().text());
  return response.json();
}
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
}
async function waitFor(expression) {
  for (let index = 0; index < 300; index++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression);
}
async function clickText(text) {
  await waitFor('[...document.querySelectorAll(".drawer.open button")].some(b=>b.textContent===' + JSON.stringify(text) + ' && !b.disabled && b.getClientRects().length)');
  await sleep(220);
  const point = await evaluate('(() => { const b=[...document.querySelectorAll(".drawer.open button")].find(b=>b.textContent===' + JSON.stringify(text) + '); b.scrollIntoView({block:"center"}); const r=b.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2}; })()');
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 });
}

try {
  await new Promise(resolve => textAPI.listen(0, '127.0.0.1', resolve));
  await api('/api/ai/settings', 'PATCH', { enabled: true, auto_analyze: false, base_url: 'http://127.0.0.1:' + textAPI.address().port + '/v1', model: 'fixture-text' });
  const upload = await fetch(base + '/api/books?filename=无字漫画测试.txt', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: '第一章 街道\n\nA quiet street at dusk. A lone figure walks slowly.' });
  assert.ok(upload.ok); const book = (await upload.json()).book;
  await api('/api/books/' + book.id + '/manga/config', 'PATCH', { provider: 'mock' });

  browser = spawn(browserPath, ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,900', 'about:blank'], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let index = 0; index < 100; index++) { await sleep(100); try { target = (await (await fetch('http://127.0.0.1:' + port + '/json')).json()).find(item => item.type === 'page'); if (target) break; } catch {} }
  assert.ok(target);
  ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const message = JSON.parse(event.data); if (pending.has(message.id)) { const request = pending.get(message.id); pending.delete(message.id); message.error ? request.reject(message.error) : request.resolve(message.result); } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + book.id + '&ch=0&debug=1' });
  await waitFor('!!window.__velmira && !document.querySelector("#btn-ai").disabled');
  await evaluate('document.querySelector("#btn-ai").click()');
  await waitFor('!!document.querySelector(".drawer-ai.open")');
  await clickText('漫画生成');
  await waitFor('!!document.querySelector(".manga-tabs") && document.querySelector(".manga-tabs").children.length===6');
  await clickText('无字整页');
  await waitFor('!!document.querySelector("[aria-label=无字漫画分格数]")');
  const selected = await evaluate('document.querySelector("[aria-label=无字漫画分格数]").value');
  assert.equal(selected, '4');
  await clickText('一次生成无字漫画页');
  await waitFor('!!document.querySelector(".manga-preview img") && document.querySelector(".manga-preview img").naturalWidth>0');
  await waitFor('[...document.querySelectorAll(".manga-note")].some(n=>n.textContent.includes("已生成"))');
  assert.equal(await evaluate('[...document.querySelectorAll(".manga-note")].some(n=>n.textContent.includes("第 1 格"))'), true);
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  const pages = await api('/api/books/' + book.id + '/manga/single-page');
  assert.ok(pages.pages.length >= 1);
  const latest = pages.pages[0];
  assert.equal(latest.status, 'done');
  assert.equal(latest.panels, 4);
  assert.equal(latest.spec.panels.length, 4);
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const image = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/single-page-comic.png', Buffer.from(image.data, 'base64'));
  assert.deepEqual(errors, []);
  console.log('[PASS] 无字整页：一次生成一张 4 格无字漫画、分格规划可见、无 JS 异常');
} finally {
  try { ws?.close(); } catch {}
  try { browser?.kill(); } catch {}
  textAPI.closeAllConnections(); await new Promise(resolve => textAPI.close(resolve));
  await sleep(350);
  try { fs.rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); } catch { /* Windows may still hold the profile briefly. */ }
}
