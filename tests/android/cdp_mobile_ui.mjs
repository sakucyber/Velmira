// UI-only Android smoke test. Never reads private credentials or calls providers.
// The only settings mutation is image-model order, restored in finally.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import { once } from 'node:events';

const adb = process.env.ANDROID_ADB || 'C:/Users/saku_cyber/.workbuddy/binaries/platform-tools/platform-tools/adb.exe';
const serial = process.env.ANDROID_SERIAL || '127.0.0.1:16384';
const adbRun = args => execFileSync(adb, ['-s', serial, ...args], { encoding: 'utf8', windowsHide: true });
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
let socket, sequence = 0, originalModels, restoreRequired = false;
const pending = new Map(), exceptions = [], externalRequests = [];
const pid = adbRun(['shell', 'pidof', 'dev.velmira.mobile']).trim();
assert.ok(pid, 'Start the debuggable Android app first');
try { adbRun(['forward', '--remove', 'tcp:9222']); } catch {}
adbRun(['forward', 'tcp:9222', 'localabstract:webview_devtools_remote_' + pid]);
const targets = await (await fetch('http://127.0.0.1:9222/json')).json();
const target = targets.find(t => t.type === 'page' && JSON.parse(t.description || '{}').visible) || targets.find(t => t.type === 'page');
assert.ok(target, 'No Android WebView target');
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence, timer = setTimeout(() => { pending.delete(id); reject(new Error('CDP timeout: ' + method)); }, 15000);
    pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, 'WebView JavaScript failed'); return result.result?.value;
}
async function waitFor(expression) {
  for (let attempt = 0; attempt < 120; attempt++) { if (await evaluate(expression)) return; await pause(70); }
  throw new Error('UI condition timed out: ' + expression);
}
async function click(selector) { await waitFor(`!!document.querySelector(${JSON.stringify(selector)})`); await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`); }
async function button(label) { await evaluate(`(()=>{const n=[...document.querySelectorAll('button')].find(n=>n.textContent===${JSON.stringify(label)});if(!n)throw Error('Missing button');n.click()})()`); }
async function navigate(url) { await send('Page.navigate', { url }); await waitFor('document.readyState==="complete"'); }
async function screenshot(name) {
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  fs.mkdirSync('tests/screenshots', { recursive: true }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(shot.data, 'base64'));
}
// Read only the redacted public settings response, and return only order/revision.
async function imageOrder() {
  return evaluate(`(async()=>{const r=await fetch('/api/mobile-ai/settings');if(!r.ok)throw Error('Settings API failed');const {settings}=await r.json();return {models:settings.image.models,revision:settings.revision};})()`);
}
async function restoreOrder() {
  const current = await imageOrder();
  const result = await evaluate(`(async()=>{const r=await fetch('/api/mobile-ai/settings',{method:'PATCH',headers:{'Content-Type':'application/json'},body:JSON.stringify({image:{models:${JSON.stringify(originalModels)}},revision:${JSON.stringify(current.revision)}})});return r.status;})()`);
  assert.ok(result >= 200 && result < 300, 'Restore of image model order failed');
  assert.deepEqual((await imageOrder()).models, originalModels, 'Original image model order restored'); restoreRequired = false;
}
async function checkWidth(section) {
  const result = await evaluate(`(()=>{const p=document.querySelector('.settings-${section}:not([hidden])');return {page:document.documentElement.scrollWidth<=innerWidth,section:p.scrollWidth<=p.clientWidth+1,width:innerWidth};})()`);
  assert.equal(result.width, 390); assert.equal(result.page, true, section + ': document horizontal overflow'); assert.equal(result.section, true, section + ': settings horizontal overflow');
}
try {
  socket = new WebSocket(target.webSocketDebuggerUrl); await once(socket, 'open');
  socket.onmessage = event => {
    const msg = JSON.parse(event.data), item = pending.get(msg.id);
    if (item) { pending.delete(msg.id); clearTimeout(item.timer); msg.error ? item.reject(new Error('CDP protocol error: ' + msg.error.message)) : item.resolve(msg.result); }
    else if (msg.method === 'Runtime.exceptionThrown') exceptions.push(msg.params.exceptionDetails.text);
    else if (msg.method === 'Network.requestWillBeSent') { const u = new URL(msg.params.request.url); if (['http:', 'https:'].includes(u.protocol) && u.hostname !== '127.0.0.1') externalRequests.push(u.origin); }
  };
  await send('Runtime.enable'); await send('Page.enable'); await send('Network.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 1 });
  const origin = await evaluate('location.origin'); await navigate(origin + '/?debug=1'); await waitFor('!!document.querySelector("#rail-ai")');
  await click('#rail-settings'); await click('[data-section=models]');
  await waitFor('!!document.querySelector(".mai-connection-editor") && document.querySelectorAll("[data-mobile-pool=image] .mai-pool-member").length>=2');
  assert.equal(await evaluate('document.querySelector(".mai-connection-editor").open'), false, 'Connection form defaults to collapsed');
  assert.equal(await evaluate('document.querySelector(".mobile-ai > .mai-card").querySelector("h3").textContent'), '已保存连接');
  await checkWidth('models'); await screenshot('android-mobile-ai-settings-390');
  for (const section of ['search', 'memory', 'prompts']) {
    await click('[data-section=' + section + ']'); await waitFor(`document.querySelector('.settings-${section}:not([hidden]) .mai-card')!==null`); await pause(100); await checkWidth(section);
  }
  console.log('[PASS] 390px model/search/memory/prompt settings fit the viewport; saved connections appear before the collapsed editor');
  await click('[data-section=models]'); originalModels = (await imageOrder()).models;
  assert.ok(originalModels.length >= 2, 'Need two existing image models for a reversible drag test');
  await evaluate('document.querySelector("[data-mobile-pool=image] .mai-pool-member").scrollIntoView({block:"center"})'); await pause(150);
  const points = await evaluate(`(()=>{const rows=[...document.querySelectorAll('[data-mobile-pool=image] .mai-pool-member')];return rows.slice(0,2).map(row=>{const r=row.querySelector('.mai-grip').getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};});})()`);
  assert.ok(points.every(p => p.x > 0 && p.x < 390 && p.y > 0 && p.y < 844), 'Both drag handles are visible');
  await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ ...points[0], id: 1 }] });
  for (let i = 1; i <= 8; i++) { await send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: points[0].x, y: points[0].y + (points[1].y - points[0].y) * i / 8, id: 1 }] }); await pause(35); }
  await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await waitFor('document.querySelector("[data-mobile-pool=image] .mai-report").textContent.includes("顺序已调整")');
  restoreRequired = true; await button('保存生图模型池');
  await waitFor('document.querySelector("[data-mobile-pool=image] .mai-report").textContent.includes("已保存")');
  const moved = (await imageOrder()).models, expected = structuredClone(originalModels); [expected[0], expected[1]] = [expected[1], expected[0]];
  assert.deepEqual(moved, expected, 'Actual touch drag swaps first and second models after saving');
  await screenshot('android-mobile-ai-drag-390'); await restoreOrder();
  console.log('[PASS] Real touch input reordered the image model pool, save persisted it, and a models-only PATCH restored the original order');
  await click('.drawer-settings .drawer-close'); await pause(150); await click('#rail-ai');
  await waitFor('!!document.querySelector(".drawer-mobile-ai [aria-label=画面描述]")');
  await waitFor('document.querySelectorAll(".mai-images img").length>0');
  await evaluate(`(()=>{const pane=document.querySelector('.drawer-mobile-ai .drawer-body'),image=document.querySelector('.mai-images img');pane.scrollTop+=image.getBoundingClientRect().top-pane.getBoundingClientRect().top-12;})()`);
  await waitFor('document.querySelector(".mai-images img").complete && document.querySelector(".mai-images img").naturalWidth>0');
  await evaluate(`(()=>{const pane=document.querySelector('.drawer-mobile-ai .drawer-body'),image=document.querySelector('.mai-images img');pane.scrollTop+=image.getBoundingClientRect().top-pane.getBoundingClientRect().top-12;})()`); await pause(150); await screenshot('android-mobile-ai-history-390');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  const book = await evaluate(`(async()=>{const r=await fetch('/api/books');const data=await r.json();return data.books.find(b=>b.format!=='cbz')?.id||null;})()`);
  if (book) {
    await navigate(origin + '/?book=' + encodeURIComponent(book) + '&debug=1'); await waitFor('!!window.__velmira');
    await click('#btn-ai'); await waitFor('!!document.querySelector(".drawer-mobile-ai [aria-label=参考作品]")');
    assert.equal(await evaluate('document.querySelector(".drawer-mobile-ai [aria-label=参考作品]").value'), book);
    console.log('[PASS] Reader AI entry opens the studio with its current book selected');
  }
  assert.deepEqual(exceptions, []); assert.deepEqual(externalRequests, []);
  console.log('[PASS] No provider calls or JavaScript exceptions occurred; existing credentials were not read or patched');
} finally {
  if (restoreRequired && originalModels && socket?.readyState === WebSocket.OPEN) await restoreOrder();
  if (socket?.readyState === WebSocket.OPEN) {
    await send('Emulation.clearDeviceMetricsOverride').catch(() => {});
    await send('Emulation.setTouchEmulationEnabled', { enabled: false }).catch(() => {});
  }
  socket?.close();
}
