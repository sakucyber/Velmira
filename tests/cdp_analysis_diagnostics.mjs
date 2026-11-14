// Verify the analysis failure panel, no-model diagnosis and support export.
// Run against a disposable server: node tests/cdp_analysis_diagnostics.mjs --base http://127.0.0.1:8899
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8899';
const port = Number(process.env.CDP_PORT || 9361);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-analysis-'));
const pending = new Map(), errors = [];
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0;

const browserPath = process.env.EDGE_PATH
  || ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
      'C:/Program Files/Google/Chrome/Application/chrome.exe']
      .find(candidate => fs.existsSync(candidate));
assert.ok(browserPath, 'Chrome or Edge is required');

async function api(url, method = 'GET', data, headers) {
  const response = await fetch(base + url, { method, body: data && JSON.stringify(data), headers: headers || (data ? { 'Content-Type': 'application/json' } : undefined) });
  assert.ok(response.ok, method + ' ' + url + ': ' + response.status + ' ' + await response.clone().text());
  return response.json();
}
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function waitFor(expression) {
  for (let index = 0; index < 240; index++) {
    if (await evaluate(expression)) return;
    await sleep(100);
  }
  throw new Error('Timeout: ' + expression);
}
async function clickText(text) {
  await waitFor('[...document.querySelectorAll(".drawer.open button")].some(b=>b.textContent===' + JSON.stringify(text) + ' && !b.disabled && b.getClientRects().length)');
  await sleep(180);
  const point = await evaluate('(() => { const b=[...document.querySelectorAll(".drawer.open button")].find(b=>b.textContent===' + JSON.stringify(text) + '); b.scrollIntoView({block:"center"}); const r=b.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2}; })()');
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 });
}

try {
  // A connection-refused endpoint fails fast and never sends a real key anywhere.
  await api('/api/ai/settings', 'PATCH', { base_url: 'https://127.0.0.1:1/v1', model: 'offline-probe', enabled: true, api_key: 'diagnostic-test-key' });
  const upload = await fetch(base + '/api/books?filename=Analysis-fixture.txt', {
    method: 'POST', headers: { 'Content-Type': 'application/octet-stream' },
    body: '第一章 起点\n\n' + '这是一段用于诊断面板验证的中性测试正文。'.repeat(60),
  });
  assert.ok(upload.ok, 'upload: ' + upload.status);
  const id = (await upload.json()).book.id;
  await api('/api/books/' + id + '/ai/analysis', 'POST');
  // Wait for the worker to record the failure before opening the panel.
  for (let index = 0; index < 120; index++) {
    const status = (await api('/api/books/' + id + '/ai/analysis')).analysis;
    if (status.status === 'error') break;
    await sleep(250);
  }

  browser = spawn(browserPath, ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port,
    '--user-data-dir=' + profile, '--window-size=1280,900', 'about:blank'], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let index = 0; index < 100; index++) {
    await sleep(100);
    try { target = (await (await fetch('http://127.0.0.1:' + port + '/json')).json()).find(item => item.type === 'page'); if (target) break; } catch {}
  }
  assert.ok(target);
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) { const request = pending.get(message.id); pending.delete(message.id); message.error ? request.reject(message.error) : request.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
  };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + id + '&debug=1' });
  await waitFor('!!window.__velmira && !document.querySelector("#btn-ai").disabled');
  await evaluate('document.querySelector("#btn-ai").click()');
  await waitFor('!!document.querySelector(".drawer-ai.open")');
  await clickText('漫画生成');
  await waitFor('!!document.querySelector(".manga-tabs")');
  await clickText('整书分析');
  await waitFor('!!document.querySelector("progress[aria-label=整书阅读进度]")');
  await waitFor('!!document.querySelector(".automatic-diagnosis-item:not([hidden])") && document.querySelector(".automatic-diagnosis-item").textContent.includes("章节分析")');
  const failureText = await evaluate('document.querySelector(".automatic-diagnosis-item").textContent');
  assert.ok(/阶段：章节分析|模型/.test(failureText), failureText);
  await clickText('诊断');
  await waitFor('[...document.querySelectorAll(".automatic-diagnosis-item")].length >= 3');
  const diagnosisText = await evaluate('document.querySelectorAll(".automatic-diagnosis-item")[1].textContent');
  assert.ok(diagnosisText.length > 4, diagnosisText);
  await waitFor('!!document.querySelector(".automatic-log")');
  const exportInfo = await evaluate('(async()=>{const r=await fetch(' + JSON.stringify(base + '/api/books/' + id + '/ai/analysis/diagnostics/export') + ');const b=await r.arrayBuffer();return {status:r.status,type:r.headers.get("content-type"),bytes:b.byteLength};})()');
  assert.equal(exportInfo.status, 200);
  assert.ok(exportInfo.type.includes('zip'), exportInfo.type);
  assert.ok(exportInfo.bytes > 500, JSON.stringify(exportInfo));
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const image = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/analysis-diagnostics.png', Buffer.from(image.data, 'base64'));
  assert.deepEqual(errors, []);
  console.log('[PASS] 失败阶段与原因可见、诊断不调用模型、任务日志、诊断包导出');
} finally {
  try { ws?.close(); } catch {}
  try { browser?.kill(); } catch {}
}
