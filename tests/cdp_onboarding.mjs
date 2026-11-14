// A standalone fixture serves the real shell, without a computer backend.
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import assert from 'node:assert/strict';

const root = path.resolve('web'), temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-onboarding-'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const requests = [], errors = [], pending = new Map();
let browser, ws, sequence = 0;
const fixture = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, 'http://fixture'); requests.push(url.pathname);
    if (url.pathname.startsWith('/api/')) {
      const body = url.pathname === '/api/auth/status' ? { local: true, authenticated: true, enabled: false, standalone: true }
        : url.pathname === '/api/books' ? { books: [] } : null;
      response.writeHead(body ? 200 : 404, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
      response.end(JSON.stringify(body || { error: { code: 'standalone_unavailable', message: '独立书库无需此接口' } })); return;
    }
    const filename = path.resolve(root, '.' + decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname));
    assert.ok(filename.startsWith(root + path.sep));
    const bytes = await fs.promises.readFile(filename);
    response.writeHead(200, { 'Content-Type': ({ '.js': 'application/javascript', '.css': 'text/css', '.html': 'text/html', '.png': 'image/png', '.webmanifest': 'application/manifest+json' })[path.extname(filename)] || 'application/octet-stream' }); response.end(bytes);
  } catch { response.writeHead(404); response.end('Not found'); }
});
function send(method, params = {}) {
  const id = ++sequence; return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
}
async function waitFor(expression) {
  for (let attempt = 0; attempt < 150; attempt++) { if (await evaluate(expression)) return; await sleep(70); }
  throw new Error('Timeout: ' + expression + '\n' + await evaluate('document.body.textContent.slice(0,1800)'));
}
async function click(selector) { await waitFor(`!!document.querySelector(${JSON.stringify(selector)})`); await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`); }
async function button(label) { await evaluate(`(()=>{const node=[...document.querySelectorAll('button')].find(n=>n.textContent===${JSON.stringify(label)});if(!node)throw new Error('Missing '+${JSON.stringify(label)});node.click();})()`); }
async function screenshot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const shot = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(shot.data, 'base64'));
}
async function reload() { await evaluate('window.__lib=null'); await send('Page.reload'); await waitFor('!!window.__lib'); }
try {
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening'); const base = 'http://127.0.0.1:' + fixture.address().port;
  const socket = net.createServer(); socket.listen(0, '127.0.0.1'); await once(socket, 'listening'); const debugPort = socket.address().port; await new Promise(resolve => socket.close(resolve));
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + debugPort, '--user-data-dir=' + path.join(temporary, 'browser'), 'about:blank'], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let attempt = 0; attempt < 100; attempt++) { try { target = (await (await fetch('http://127.0.0.1:' + debugPort + '/json')).json()).find(item => item.type === 'page'); if (target) break; } catch {} await sleep(100); }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const message = JSON.parse(event.data), item = pending.get(message.id); if (item) { pending.delete(message.id); message.error ? item.reject(message.error) : item.resolve(message.result); } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await send('Page.navigate', { url: base + '/?debug=1' });
  await waitFor('!!document.querySelector(".onboarding")');
  assert.equal(await evaluate('document.querySelectorAll(".onboarding input:not([type=range])").length'), 0);
  assert.equal(await evaluate('document.querySelector(".onboarding").textContent.includes("配对")'), false);
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await screenshot('onboarding-first-mobile');
  await button('跳过，直接进入'); await waitFor('!!window.__lib');
  assert.equal(await evaluate('!!document.querySelector(".onboarding")'), false);
  assert.equal(await evaluate('localStorage.getItem("velmira.onboarding.v1")'), '1');
  console.log('[PASS] Fresh standalone launch shows optional personalization without an address or credentials; skip opens the local library');
  await reload(); assert.equal(await evaluate('!!document.querySelector(".onboarding")'), false);
  await click('#rail-settings'); await waitFor('!!document.querySelector(".drawer-settings.open")');
  assert.equal(await evaluate('document.querySelectorAll(".settings-tabs button").length'), 7);
  await click('.settings-tabs [data-section=offline]');
  await waitFor('!!document.querySelector(".standalone-library-settings")');
  assert.ok(await evaluate('document.querySelector(".standalone-library-settings").textContent.includes("无需再次下载")'));
  assert.equal(await evaluate('document.querySelectorAll(".standalone-library-settings input").length'), 0);
  await click('.settings-tabs [data-section=security]'); await waitFor('!!document.querySelector(".standalone-connection-settings")');
  assert.equal(await evaluate('document.querySelector(".standalone-connection-settings a").getAttribute("href")'), 'velmira://connect');
  assert.ok(await evaluate('document.querySelector(".standalone-connection-settings").textContent.includes("不会自动合并")'));
  await click('.settings-tabs [data-section=reading]'); await button('重新个性化'); await waitFor('!!document.querySelector(".onboarding")');
  await click('.onboarding [aria-label="夜间主题"]');
  await evaluate('(()=>{const n=document.querySelector("[aria-label=首次阅读字号]");n.value=23;n.dispatchEvent(new Event("input",{bubbles:true}));const d=document.querySelector("[aria-label=首次书架密度]");d.value="compact";d.dispatchEvent(new Event("change",{bubbles:true}));})()');
  assert.equal(await evaluate('document.querySelector(".onboarding-preview").style.fontSize'), '23px');
  await button('开始阅读'); await waitFor('!!window.__lib && !document.querySelector(".onboarding") && document.documentElement.dataset.theme==="night"');
  await waitFor('JSON.parse(localStorage.getItem("velmira.shelf")).density==="compact"');
  assert.equal(await evaluate('JSON.parse(localStorage.getItem("velmira.r1")).fontSize'), 23);
  await reload(); assert.equal(await evaluate('!!document.querySelector(".onboarding")'), false);
  assert.equal(await evaluate('document.documentElement.dataset.theme'), 'night');
  console.log('[PASS] Personalization reopens from settings, persists theme/font/shelf density and stays dismissed after restart');
  await evaluate('(async()=>{localStorage.removeItem("velmira.onboarding.v1");await (await import("/util/local-store.js")).localStore.meta("velmira.onboarding.v1",false);return true;})()');
  await send('Page.reload'); await waitFor('!!document.querySelector(".onboarding")');
  await click('.onboarding [aria-label="羊皮纸主题"]'); await button('开始阅读'); await waitFor('!!window.__lib && !document.querySelector(".onboarding")');
  assert.equal(await evaluate('document.documentElement.dataset.theme'), 'sepia');
  await click('#rail-settings'); await waitFor('!!document.querySelector(".drawer-settings.open")');
  await click('.settings-tabs [data-section=security]'); await waitFor('!!document.querySelector(".standalone-connection-settings")');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true); await screenshot('standalone-settings-mobile');
  assert.deepEqual(errors, []);
  assert.ok(!requests.some(url => url.startsWith('/api/sync/')), 'Standalone launch never invokes the remote sync protocol');
  console.log('[PASS] Completing first-run setup opens the library; compact settings fit 390px and no remote sync or JavaScript errors occur');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  if (browser?.exitCode === null) { const killer = spawn('taskkill', ['/PID', String(browser.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true }); await once(killer, 'exit'); }
  fixture.closeAllConnections?.(); await new Promise(resolve => fixture.close(resolve));
  const target = path.resolve(temporary), parent = path.resolve(os.tmpdir()) + path.sep;
  assert.ok(target.startsWith(parent) && path.basename(target).startsWith('velmira-onboarding-'));
  await fs.promises.rm(target, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
