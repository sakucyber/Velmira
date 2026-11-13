import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const base = process.argv.includes('--base') ? process.argv[process.argv.indexOf('--base') + 1] : 'http://127.0.0.1:8891';
const port = 9341;
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-home-settings-check-'));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let browser, ws, sequence = 0;
const pending = new Map();
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
  for (let i = 0; i < 100; i++) { if (await evaluate(expression)) return; await sleep(50); }
  throw new Error('Timeout ' + expression);
}

try {
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', [
    '--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port,
    '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank',
  ], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) {
    await sleep(100);
    try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((item) => item.type === 'page'); if (target) break; } catch {}
  }
  assert.ok(target, 'Browser started');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve) => { ws.onopen = resolve; });
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) { const p = pending.get(message.id); pending.delete(message.id); message.error ? p.reject(message.error) : p.resolve(message.result); }
  };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?debug=1' });
  await waitFor('!!window.__lib && !!document.querySelector("#rail-settings")'); await sleep(300);
  assert.equal(await evaluate('document.querySelector("#rail-settings").textContent.includes("设置")'), true);
  assert.equal(await evaluate('getComputedStyle(document.querySelector("#rail-settings")).position'), 'static');
  const desktop = await evaluate('(() => { const n = document.querySelector("#rail-settings"); const r = n.getBoundingClientRect(); const rail = document.querySelector(".app-rail").getBoundingClientRect(); return { bottom: r.bottom, railBottom: rail.bottom }; })()');
  assert.ok(desktop.bottom > desktop.railBottom - 100, JSON.stringify(desktop));
  await evaluate('document.querySelector("#rail-settings").click()');
  await waitFor('!!document.querySelector(".drawer-settings.open")');
  assert.ok(await evaluate('[...document.querySelectorAll(".settings-tabs button")].some((n) => n.textContent.includes("阅读"))'));
  assert.ok(await evaluate('[...document.querySelectorAll(".settings-tabs button")].some((n) => n.textContent.includes("漫画"))'));
  await evaluate('document.querySelector(".drawer-close").click()'); await waitFor('document.querySelector("#drawer-layer").hidden');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await send('Page.navigate', { url: base + '/?debug=1' }); await waitFor('!!window.__lib && !!document.querySelector("#rail-settings")'); await sleep(300);
  const mobile = await evaluate('(() => { const n = document.querySelector("#rail-settings"); const r = n.getBoundingClientRect(); return { position: getComputedStyle(n).position, left: r.left, bottom: innerHeight - r.bottom, width: document.documentElement.scrollWidth }; })()');
  assert.equal(mobile.position, 'fixed'); assert.ok(mobile.left <= 16, JSON.stringify(mobile)); assert.ok(mobile.bottom >= 10 && mobile.bottom <= 24, JSON.stringify(mobile)); assert.equal(mobile.width, 390);
  console.log('[PASS] 主页左下角设置按钮在桌面端/手机端定位正确，设置抽屉可打开');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(200);
  fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}
