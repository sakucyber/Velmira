/* Long-paragraph progress must survive page turns, annotations, reloads and reflow.
   Run against a disposable DATA_DIR: node tests/cdp_progress.mjs --base http://127.0.0.1:8893 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8893';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-progress-'));
const port = 9343, pending = new Map(), errors = [];
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0;
function send(method, params = {}) {
  return new Promise((resolve, reject) => { const id = ++sequence; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
}
async function waitFor(expression) {
  for (let i = 0; i < 100; i++) { if (await evaluate(expression)) return; await sleep(80); }
  throw new Error('Timeout: ' + expression);
}
async function nav(url) {
  await send('Page.navigate', { url }); await waitFor('!!window.__velmira'); await sleep(350);
}
async function api(url, method = 'GET', data) {
  const response = await fetch(base + url, { method, headers: data ? { 'Content-Type': 'application/json' } : {}, body: data ? JSON.stringify(data) : undefined });
  assert.ok(response.ok, `${method} ${url}: ${response.status}`); return response.json();
}
const location = () => evaluate('({para:__velmira.state.para, offset:__velmira.state.offset, page:__velmira.pager.page})');
try {
  const long = '这是一段用于验证跨页位置的中性文字。🌙我们继续阅读。'.repeat(200);
  const imported = await fetch(base + '/api/books?filename=progress-demo.txt', { method: 'POST', body: '第一章 测试\n\n' + long + '\n\n短段落。\n\n第二章 后续\n\n结束测试。' + Date.now() });
  assert.equal(imported.status, 201); const book = (await imported.json()).book;
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(80); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => {
    const msg = JSON.parse(event.data);
    if (pending.has(msg.id)) { const p = pending.get(msg.id); pending.delete(msg.id); msg.error ? p.reject(msg.error) : p.resolve(msg.result); }
    if (msg.method === 'Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.text);
  };
  await send('Runtime.enable'); await send('Page.enable');
  const url = base + '/?book=' + book.id + '&debug=1';
  await nav(url);
  const snippet = await evaluate(`(async () => {
    const {renderSnippet}=await import('/ui/panels.js'); const n=document.createElement('span');
    renderSnippet(n,'<img src=x onerror=alert(1)><b>&lt;safe&gt;</b>&amp;lt;');
    return {text:n.textContent,b:n.querySelector('b').textContent,elements:[...n.querySelectorAll('*')].map(e=>e.tagName)};
  })()`);
  assert.deepEqual(snippet, {text:'<img src=x onerror=alert(1)><safe>&lt;',b:'<safe>',elements:['B']});
  console.log('[PASS] 搜索高亮仅创建文本与 b 节点，未知 HTML 不执行');
  assert.ok(await evaluate('__velmira.pager.pages > 4 && !__velmira.pager.vertical'));
  await evaluate('__velmira.next(true)'); const first = await location();
  assert.equal(first.para, 0); assert.ok(first.offset > 0, JSON.stringify(first));
  await sleep(250); assert.equal(await evaluate('__velmira.pager.locateOffset()'), first.offset);
  await evaluate('__velmira.next(true)'); const saved = await location();
  assert.ok(saved.offset > first.offset); assert.equal(saved.para, 0);
  await sleep(1600);
  const progress = (await api('/api/books/' + book.id + '/progress')).progress;
  assert.equal(progress.offset, saved.offset);
  console.log('[PASS] 跨页长段落和翻页动画保存实际字符 offset（含非 BMP 字符）');

  const link = await evaluate('location.href');
  await nav(link + '&debug=1'); assert.deepEqual(await location(), saved);
  await nav(url); assert.deepEqual(await location(), saved);
  await evaluate('localStorage.clear()'); await nav(url); assert.deepEqual(await location(), saved);
  console.log('[PASS] URL、localStorage、服务端三种恢复均回到同一页');

  await evaluate('__velmira.toggleBookmark()');
  const bookmark = (await api('/api/books/' + book.id + '/bookmarks')).bookmarks[0];
  assert.equal(bookmark.offset, saved.offset);
  await evaluate('__velmira.pager.goToPage(0)'); await evaluate('__velmira.jumpTo(' + JSON.stringify(bookmark) + ')');
  assert.equal((await location()).page, saved.page);
  // Splitting DOM text into marks must preserve character anchors.
  await evaluate(`__velmira.pager.setAnnotations([{id:'test',para:0,start_offset:5,end_offset:90,note:'note',color:'yellow'}])`);
  await evaluate('__velmira.pager.relocate()'); assert.equal((await location()).page, saved.page);
  console.log('[PASS] 书签使用段内位置，批注拆分文本节点不改变页首');

  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await sleep(700);
  const visible = await evaluate(`(() => {
    const p=__velmira.pager, node=p.column.querySelector('[data-i="0"]');
    const units=Array.from(node.textContent).slice(0,${saved.offset}).join('').length;
    const r=p._charRectFromText(p._textNodes(node),units), vp=p.viewport.getBoundingClientRect();
    return r.left>=vp.left && r.right<=vp.right && r.top>=vp.top && r.bottom<=vp.bottom;
  })()`);
  assert.ok(visible, 'saved character remains visible after mobile reflow');
  assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true);
  console.log('[PASS] 390px 重排以保存的字符定位且无横向溢出');

  await evaluate('(async()=>{const p=__velmira.pager;await p._enableVertical(0);p.viewport.scrollTop=700;p.emit()})()');
  const vertical = await location(); assert.ok(vertical.offset > 0);
  await evaluate('__velmira.pager.gotoPara(0, ' + vertical.offset + ')');
  const restored = await location(); assert.equal(restored.offset, vertical.offset);
  assert.deepEqual(errors, []);
  console.log('[PASS] 滚动模式保存恢复段内字符，全程零 JS 异常');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(200);
  fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}
