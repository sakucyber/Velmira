/* Neutral fixture + real API/browser regression for annotations and library imports.
   node tests/cdp_annotations.mjs --base http://127.0.0.1:8876 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const opt = (key, fallback) => args.includes(key) ? args[args.indexOf(key) + 1] : fallback;
const base = opt('--base', 'http://127.0.0.1:8876');
const port = Number(opt('--port', '9336'));
const edge = process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-ann-'));
const shots = path.resolve('tests/screenshots');
fs.mkdirSync(shots, { recursive: true });

async function api(url, method = 'GET', body) {
  const result = await fetch(base + url, { method, body: body && JSON.stringify(body),
    headers: body ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(result.ok, `${method} ${url}: ${result.status}`);
  return result.json();
}

let browser, ws;
const pending = new Map();
const errors = [];
let fileChooserOpened = false;
let sequence = 0;
function send(method, params = {}) {
  const id = ++sequence;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function waitFor(expression) {
  for (let attempt = 0; attempt < 60; attempt++) {
    if (await evaluate(expression)) return;
    await sleep(100);
  }
  throw new Error('Timeout: ' + expression);
}
async function navigate(url) {
  await send('Page.navigate', { url });
  await waitFor("!!document.querySelector('#reader-column mark.ann-note') && !!window.__velmira");
  await sleep(500);
}
async function point(selector) {
  return evaluate(`(() => { const node=document.querySelector(${JSON.stringify(selector)});
    const r=[...node.getClientRects()].find(r=>r.left>=0&&r.right<=innerWidth&&r.top>=0&&r.bottom<=innerHeight);
    if (!r) throw new Error('No visible rect'); return {x:r.left+r.width/2,y:r.top+r.height/2}; })()`);
}
async function click(selector, touch = false) {
  const { x, y } = await point(selector);
  if (touch) {
    await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y, id: 1 }] });
    await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  } else {
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
  }
}
async function screenshot(name) {
  await sleep(250); // Capture the settled popover after its entrance animation.
  const result = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(path.join(shots, name), Buffer.from(result.data, 'base64'));
}
function pass(label) { console.log('[PASS] ' + label); }

try {
  // Import into the caller's isolated test server, never the user's library.
  const imported = await fetch(base + '/api/books?filename=annotation-demo.txt&force=1', {
    method: 'POST', headers: { 'Content-Type': 'application/octet-stream' },
    body: fs.readFileSync('samples/demo-text.txt', 'utf8') + '\n回归标识 ' + Date.now(),
  });
  assert.equal(imported.status, 201);
  const book = (await imported.json()).book;
  const chapter = await api(`/api/books/${book.id}/chapters/0`);
  const note = '这是一条示例批注。\n支持换行与长内容。<img src=x onerror=alert(1)>';
  const annotation = (await api(`/api/books/${book.id}/annotations`, 'POST', {
    chapter: 0, para: 0, start_offset: 0, end_offset: 6,
    text: chapter.paragraphs[0].slice(0, 6), color: 'yellow', note,
  })).annotation;

  browser = spawn(edge, ['--headless=new', '--disable-gpu', '--no-first-run',
    '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'],
  { stdio: 'ignore' });
  let target;
  for (let attempt = 0; attempt < 80; attempt++) {
    await sleep(100);
    try {
      target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page');
      if (target) break;
    } catch { /* browser starting */ }
  }
  assert.ok(target, 'Edge started');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  ws.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.id && pending.has(message.id)) {
      const promise = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) promise.reject(new Error(JSON.stringify(message.error))); else promise.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
    else if (message.method === 'Page.fileChooserOpened') fileChooserOpened = true;
  };
  await send('Page.enable');
  await send('Runtime.enable');
  await navigate(base + `/?book=${book.id}&ch=0&debug=1`);
  assert.equal(await evaluate("getComputedStyle(document.querySelector('mark.ann-note')).textDecorationStyle"), 'double');
  pass('有笔记的文字显示双下划线');
  const hover = await point('mark.ann-note');
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...hover });
  await waitFor("!!document.querySelector('.ann-note-content')");
  assert.equal(await evaluate("document.querySelector('.ann-note-content').textContent"), note);
  assert.equal(await evaluate("document.querySelectorAll('.ann-note-content img').length"), 0);
  pass('鼠标悬停直接显示完整批注，HTML 作为文字展示');
  await screenshot('annotation-desktop.png');
  // Move away from both the mark and the popover. The popover is clamped to
  // the top-left when the annotated line is the first line, so the old (20,20)
  // target stayed inside the popover and correctly kept it open.
  const away = await evaluate('({x: Math.round(innerWidth / 2), y: Math.round(innerHeight - 60)})');
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...away });
  await sleep(300);
  assert.equal(await evaluate("!!document.querySelector('#ann-popover')"), false);
  pass('鼠标移开自动收起');

  const before = await evaluate('location.search');
  await click('mark.ann-note');
  await waitFor("!!document.querySelector('.ann-note-content')");
  assert.equal(await evaluate('location.search'), before);
  pass('鼠标点击固定弹层且不翻页');
  await evaluate("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
  assert.equal(await evaluate("!!document.querySelector('#ann-popover')"), false);
  await evaluate("document.querySelector('mark.ann-note').focus(); document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}))");
  assert.equal(await evaluate("!!document.querySelector('.ann-note-content')"), true);
  pass('键盘聚焦后 Enter 查看批注');

  // Edit via the actual note dialog and save through the API.
  await evaluate("[...document.querySelectorAll('.ann-act')].find(b=>b.textContent==='编辑').click()");
  await waitFor("!!document.querySelector('.note-input')");
  await evaluate("document.querySelector('.note-input').value='更新后的示例批注'; document.querySelector('.note-actions .btn-primary').click()");
  await waitFor("!document.querySelector('.note-layer') && window.__velmira.annotations().some(a=>a.note==='更新后的示例批注')");
  assert.equal((await api(`/api/books/${book.id}/annotations?chapter=0`)).annotations[0].note, '更新后的示例批注');
  await navigate(base + `/?book=${book.id}&ch=0&debug=1`);
  await click('mark.ann-note');
  assert.equal(await evaluate("document.querySelector('.ann-note-content').textContent"), '更新后的示例批注');
  pass('编辑保存后刷新仍显示批注');

  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true });
  await navigate(base + `/?book=${book.id}&ch=0&debug=1`);
  const mobileBefore = await evaluate('location.search');
  await click('mark.ann-note', true);
  await waitFor("!!document.querySelector('.ann-note-content')");
  assert.equal(await evaluate('location.search'), mobileBefore);
  assert.ok(await evaluate("(() => {const r=document.querySelector('#ann-popover').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth&&r.top>=0&&r.bottom<=innerHeight&&document.documentElement.scrollWidth===390})()"));
  await screenshot('annotation-mobile.png');
  pass('390×844 触摸点击显示批注，不翻页且无溢出');
  await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 195, y: 650, id: 1 }] });
  await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  assert.equal(await evaluate("!!document.querySelector('#ann-popover')"), false);
  pass('手机点击空白关闭');

  await api(`/api/books/${book.id}/annotations/${annotation.id}`, 'PATCH', { note: null });
  await send('Page.reload');
  await waitFor("!!document.querySelector('mark.ann')");
  assert.equal(await evaluate("document.querySelectorAll('mark.ann-note').length"), 0);
  await sleep(500);
  await click('mark.ann', true);
  assert.equal(await evaluate("!!document.querySelector('.ann-note-content')"), false);
  assert.equal(await evaluate("!!document.querySelector('.ann-toolbar')"), true);
  pass('清空笔记恢复普通高亮，编辑入口保留');

  await send('Page.navigate', { url: base + '/?debug=1' });
  await waitFor("!!window.__lib && document.querySelectorAll('.book-card').length>0");
  const dom = await send('DOM.getDocument');
  const input = await send('DOM.querySelector', { nodeId: dom.root.nodeId, selector: '#file-input' });
  await send('DOM.setFileInputFiles', { nodeId: input.nodeId, files: [path.resolve('samples/demo-text.txt')] });
  await waitFor("location.search.includes('book=') && !document.querySelector('#reader-view').hidden");
  await evaluate('history.back()');
  await waitFor("!document.querySelector('#library-view').hidden && !!window.__lib");
  await send('Page.setInterceptFileChooserDialog', { enabled: true });
  await click('#rail-import', true);
  await sleep(200);
  assert.ok(fileChooserOpened, 'File chooser opens again after returning from a successful import');
  pass('导入成功后返回书架，可再次打开文件选择器');
  await send('Page.setInterceptFileChooserDialog', { enabled: false });

  // Use the browser's real file-drag input path, including drops over controls.
  await send('Emulation.setTouchEmulationEnabled', { enabled: false });
  await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 860, deviceScaleFactor: 1, mobile: false });
  const dragData = { items: [], files: [path.resolve('samples/demo-text.txt')], dragOperationsMask: 1 };
  for (const selector of ['#nav-library', '#lib-filter', '.book-cover', '.lib-heading']) {
    const target = await point(selector);
    await send('Input.dispatchDragEvent', { type: 'dragEnter', ...target, data: dragData });
    await send('Input.dispatchDragEvent', { type: 'dragOver', ...target, data: dragData });
    assert.equal(await evaluate("document.querySelector('#library-drop-overlay').hidden"), false);
    await send('Input.dispatchDragEvent', { type: 'drop', ...target, data: dragData });
    await waitFor("location.search.includes('book=') && !document.querySelector('#reader-view').hidden");
    await evaluate('history.back()');
    await waitFor("!document.querySelector('#library-view').hidden && !!window.__lib");
    assert.equal(await evaluate("document.querySelector('#library-drop-overlay').hidden"), true);
  }
  pass('文件拖到侧栏、搜索框、封面或标题空白处均可导入，返回后可继续拖入');

  const textDrag = await evaluate(`(() => {
    const data = new DataTransfer(); data.setData('text/plain', '示例文字');
    const event = new DragEvent('dragover', {bubbles:true,cancelable:true,dataTransfer:data});
    document.querySelector('#lib-filter').dispatchEvent(event);
    return {prevented:event.defaultPrevented,overlay:!document.querySelector('#library-drop-overlay').hidden};
  })()`);
  assert.deepEqual(textDrag, { prevented: false, overlay: false });
  pass('普通文字拖拽不触发文件导入提示');

  const cancelPoint = await point('#lib-filter');
  await send('Input.dispatchDragEvent', { type: 'dragEnter', ...cancelPoint, data: dragData });
  await send('Input.dispatchDragEvent', { type: 'dragOver', ...cancelPoint, data: dragData });
  await screenshot('library-file-drag.png');
  await send('Input.dispatchDragEvent', { type: 'dragCancel', ...cancelPoint, data: dragData });
  await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
  await waitFor("document.querySelector('#library-drop-overlay').hidden");
  pass('按 Escape 取消文件拖拽后提示收起，页面保持书架');
  assert.deepEqual(errors, []);
  pass('全程无 JavaScript 异常');
} finally {
  if (ws && ws.readyState === WebSocket.OPEN) {
    await send('Browser.close').catch(() => {});
    ws.close();
  }
  if (browser) browser.kill();
  await sleep(200);
  fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}
