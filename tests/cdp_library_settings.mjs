/* Real API + isolated browser checks. Run only against a disposable DATA_DIR.
   node tests/cdp_library_settings.mjs --base http://127.0.0.1:8881 */
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8881';
const port = 9337;
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-shelf-'));
const shots = path.resolve('tests/screenshots'); fs.mkdirSync(shots, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const errors = [], pending = new Map();
let browser, ws, sequence = 0;
async function api(url, method = 'GET', body) {
  const result = await fetch(base + url, { method, body: body && JSON.stringify(body), headers: body ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(result.ok, `${method} ${url}: ${result.status}`); return result.json();
}
async function upload(name, body) {
  const result = await fetch(base + '/api/books?filename=' + encodeURIComponent(name), { method: 'POST', body, headers: { 'Content-Type': 'application/octet-stream' } });
  assert.ok(result.ok, 'import ' + name); return (await result.json()).book;
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
  for (let i = 0; i < 100; i++) { if (await evaluate(expression)) return; await sleep(80); }
  throw new Error('Timeout ' + expression);
}
async function click(selector) {
  const point = await evaluate(`(() => {const n=document.querySelector(${JSON.stringify(selector)});if(!n)throw new Error('Missing control');n.scrollIntoView({block:'nearest'});const r=n.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2};})()`);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 });
}
async function select(selector, value) {
  await evaluate(`(() => {const n=document.querySelector(${JSON.stringify(selector)});n.value=${JSON.stringify(value)};n.dispatchEvent(new Event('change',{bubbles:true}));})()`);
}
async function key(value) { await send('Input.dispatchKeyEvent', { type: 'keyDown', key: value }); await send('Input.dispatchKeyEvent', { type: 'keyUp', key: value }); }
async function nav(url, ready) { await send('Page.navigate', { url }); await waitFor(ready); await sleep(500); }
async function shot(name) { const result = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync(path.join(shots, name), Buffer.from(result.data, 'base64')); }
function pass(text) { console.log('[PASS] ' + text); }

try {
  const stamp = Date.now();
  const sample = fs.readFileSync('samples/demo-text.txt', 'utf8');
  const text = await upload('山海之间.txt', sample + '\n测试编号 ' + stamp);
  await api('/api/books/' + text.id, 'PATCH', { title: '山海之间' });
  for (const title of ['暮色来信', '雨后的图书馆', '长夜与星河']) {
    const b = await upload(title + '.txt', sample + '\n' + title + stamp);
    await api('/api/books/' + b.id, 'PATCH', { title });
  }
  const comic = await upload('远方旅行.cbz', fs.readFileSync('samples/demo-comic.cbz'));
  await api('/api/books/' + comic.id, 'PATCH', { title: '远方旅行' });
  // A neutral, actual EPUB fixture rather than changing the database's format field.
  const epub = spawnSync('python', ['-c', `import io,zipfile,base64,sys
b=io.BytesIO()
with zipfile.ZipFile(b,'w') as z:
 z.writestr('mimetype','application/epub+zip')
 z.writestr('META-INF/container.xml','<container><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>')
 z.writestr('book.opf','<package xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>山海之间</dc:title></metadata><manifest><item id="c" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>')
 z.writestr('chapter.xhtml','<html><body><h1>第一章 远行</h1><p>这是一段中性的测试正文。</p></body></html>')
sys.stdout.write(base64.b64encode(b.getvalue()).decode())`], { encoding: 'utf8' });
  assert.equal(epub.status, 0, epub.stderr);
  const epubBook = await upload('山海之间.epub', Buffer.from(epub.stdout, 'base64'));
  await api('/api/books/' + text.id + '/progress', 'PUT', { chapter: 0, para: 0, offset: 1 });
  const chapter = (await api('/api/books/' + text.id + '/chapters/0')).paragraphs;
  const note = (await api('/api/books/' + text.id + '/annotations', 'POST', { chapter: 0, para: 0, start_offset: 0, end_offset: 4, text: chapter[0].slice(0, 4), note: '设置调整后应保留的批注', color: 'yellow' })).annotation;
  const activeBooks = (await api('/api/books')).books;
  const count = activeBooks.length;
  const epubs = activeBooks.filter(b => b.format === 'epub').length;
  const reading = activeBooks.filter(b => b.has_progress && b.percent < .999).length;
  const matching = activeBooks.filter(b => b.title.includes('山海')).length;

  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target, 'Browser started');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve) => { ws.onopen = resolve; });
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) { const p = pending.get(message.id); pending.delete(message.id); message.error ? p.reject(message.error) : p.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
  };
  await send('Page.enable'); await send('Runtime.enable');
  await nav(base + '/?debug=1', '!!window.__lib && document.querySelectorAll(".book-card").length>=6');
  const cover = (id) => `.book-card[data-id="${id}"] .book-cover`;
  const hues = await evaluate(`[${JSON.stringify(cover(text.id))},${JSON.stringify(cover(epubBook.id))}].map(s=>document.querySelector(s).style.getPropertyValue('--book-hue'))`);
  assert.notEqual(hues[0], hues[1]);
  assert.ok(await evaluate(`document.querySelector(${JSON.stringify(cover(epubBook.id))}).nextElementSibling.textContent.includes('字')`));
  assert.equal(await evaluate(`document.querySelector('.book-card[data-id="${comic.id}"] .cover-state').textContent`), '未读');
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(cover(text.id) + ' .cover-progress i')}).getBoundingClientRect().width>=3`), true);
  await shot('shelf-standard-day.png'); pass('真实 TXT/EPUB/CBZ、同名封面区分、统一元数据与极小进度');

  await click('#density-compact');
  assert.ok(await evaluate('document.querySelector("#library-view").classList.contains("shelf-compact")'));
  await select('#lib-format', 'epub'); assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), epubs);
  await select('#lib-format', 'all'); await select('#lib-status', 'reading'); assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), reading);
  await click('#nav-continue'); await select('#lib-status', 'unread');
  assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), activeBooks.filter(b => !b.has_progress).length);
  await click('#nav-library'); assert.equal(await evaluate('document.querySelector("#lib-status").value'), 'all');
  await select('#lib-status', 'all'); await select('#lib-sort', 'title');
  await key('/'); assert.equal(await evaluate('document.activeElement.id'), 'lib-filter');
  await evaluate('document.querySelector("#lib-filter").value="山海"; document.querySelector("#lib-filter").dispatchEvent(new Event("input",{bubbles:true}))');
  assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), matching);
  await click('#lib-clear'); assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), count);
  const sortedTitles = await evaluate('[...document.querySelectorAll(".book-title")].map(n=>n.textContent)');
  assert.deepEqual(sortedTitles, sortedTitles.slice().sort((a,b)=>a.localeCompare(b,'zh-CN',{numeric:true})));
  pass('紧凑密度、格式/阅读状态筛选、排序和 / 搜索清空');

  await click(`.book-card[data-id="${text.id}"] .book-more`); await waitFor('!!document.querySelector("dialog[open]")');
  await evaluate('document.querySelector(".rename-form input").value="山海之间·修订";document.querySelector(".rename-form").requestSubmit()');
  await waitFor(`!document.querySelector('dialog') && document.querySelector('.book-card[data-id="${text.id}"] .book-title').textContent==='山海之间·修订'`);
  await click(`.book-card[data-id="${text.id}"] .book-more`); await click('.book-details .btn-danger');
  await waitFor(`!document.querySelector('dialog') && !document.querySelector('.book-card[data-id="${text.id}"]')`);
  await send('Page.reload'); await waitFor('!!window.__lib'); await select('#lib-status', 'archived');
  assert.ok(await evaluate('document.querySelector("#library-view").classList.contains("shelf-compact")'));
  await waitFor(`!!document.querySelector('.book-card[data-id="${text.id}"]')`);
  await click(`.book-card[data-id="${text.id}"] .book-more`); await click('.book-details > .btn-primary');
  await waitFor(`!document.querySelector('dialog') && !document.querySelector('.book-card[data-id="${text.id}"]')`);
  await select('#lib-status', 'all'); assert.equal(await evaluate('document.querySelectorAll(".book-card").length'), count);
  assert.ok((await api(`/api/books/${text.id}/annotations?chapter=0`)).annotations.some(a => a.id === note.id));
  pass('详情重命名、移出后刷新仍生效、恢复后批注完整');

  await api('/api/books/' + text.id + '/progress', 'PUT', { chapter: 2, para: 0, offset: 0 });
  await evaluate('localStorage.removeItem("velmira.r1")');
  await click(`.book-card[data-id="${text.id}"] .book-more`); await click('.book-details .btn-danger');
  await waitFor(`!document.querySelector('dialog') && !document.querySelector('.book-card[data-id="${text.id}"]')`);
  const reimportFile = path.join(profile, 'reimport.txt');
  fs.writeFileSync(reimportFile, sample + '\n测试编号 ' + stamp, 'utf8');
  const doc = await send('DOM.getDocument');
  const input = await send('DOM.querySelector', { nodeId: doc.root.nodeId, selector: '#file-input' });
  await send('DOM.setFileInputFiles', { nodeId: input.nodeId, files: [reimportFile] });
  await waitFor(`location.search.includes(${JSON.stringify(text.id)}) && document.querySelector('#chapter-label')?.textContent.startsWith('第3章')`);
  assert.ok((await api('/api/books')).books.some(b => b.id === text.id));
  assert.ok((await api(`/api/books/${text.id}/annotations?chapter=0`)).annotations.some(a => a.id === note.id));
  pass('重新导入恢复原书 ID、批注，并从服务端保存的第 3 章继续');

  await evaluate(`localStorage.setItem('velmira.r1', JSON.stringify({...JSON.parse(localStorage.getItem('velmira.r1') || '{}'), fontSize: 2}))`);
  await nav(base + `/?book=${text.id}&ch=0&debug=1`, '!!window.__velmira && !!document.querySelector("mark.ann-note")');
  assert.equal(await evaluate('getComputedStyle(document.querySelector("#reader-column")).fontSize'), '22px');
  assert.deepEqual(await evaluate(`(async () => {const core=await import('/util/core.js');const saved=localStorage.getItem('velmira.r1');try{return [0,1,2,3].map(fontSize=>{localStorage.setItem('velmira.r1',JSON.stringify({fontSize}));return core.store.get().fontSize;});}finally{localStorage.setItem('velmira.r1',saved);}})()`), [17, 19, 22, 25]);
  assert.equal(await evaluate('document.querySelectorAll(".foot-hint").length'), 0);
  await click('#btn-settings'); await waitFor('!!document.querySelector(".drawer-settings.open")');
  const before = await evaluate('window.__velmira.state.page');
  await key('ArrowRight'); assert.equal(await evaluate('window.__velmira.state.page'), before);
  await click('[data-theme-choice="night"]');
  await click('.settings-font-range');
  await waitFor('parseFloat(getComputedStyle(document.querySelector("#reader-column")).fontSize)>25');
  await select('[aria-label="阅读字号"]', '23.7');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "23.7px"');
  await evaluate('document.querySelector(".settings-font-range").focus()');
  await key('ArrowRight');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "23.8px"');
  await evaluate(`(() => { const range=document.querySelector('.settings-font-range'); for(const value of [30.2,18.4,27.6]) {range.value=value;range.dispatchEvent(new Event('input',{bubbles:true}));} })()`);
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "27.6px"');
  await sleep(150);
  assert.equal(await evaluate('document.querySelector("[aria-label=阅读字号]").value'), '27.6');
  await select('[aria-label="阅读字号"]', '500');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "48px"');
  await select('[aria-label="阅读字号"]', '');
  assert.equal(await evaluate('document.querySelector("[aria-label=阅读字号]").value'), '48');
  await click('.settings-font-foot button');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "19px"');
  await select('[aria-label="阅读字号"]', '23.7');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "23.7px"');
  await sleep(150);
  assert.equal(await evaluate('document.documentElement.dataset.theme'), 'night');
  assert.equal(await evaluate('document.querySelectorAll("mark.ann-note").length>0'), true);
  await shot('reader-settings-desktop.png');
  await key('Escape'); await waitFor('document.querySelector("#drawer-layer").hidden');
  assert.equal(await evaluate('document.activeElement.id'), 'btn-settings');
  // The reader canonicalizes its URL without debug=1; navigate with it for the debug hook.
  await nav(base + `/?book=${text.id}&debug=1`, '!!window.__velmira');
  assert.equal(await evaluate('getComputedStyle(document.querySelector("#reader-column")).fontSize'), '23.7px');
  assert.equal(await evaluate('document.documentElement.dataset.theme'), 'night');
  await key('t'); await waitFor('!!document.querySelector(".drawer-toc.open")'); await key('Escape');
  await waitFor('document.querySelector("#drawer-layer").hidden');
  await key('f'); await waitFor('!!document.querySelector(".drawer-search.open")'); await key('Escape');
  await waitFor('document.querySelector("#drawer-layer").hidden');
  pass('旧字号兼容、连续滑块/小数输入/快速调整/边界/重置、持久化、批注和快捷键');

  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await nav(base + `/?book=${text.id}&ch=0&debug=1`, '!!window.__velmira'); await click('#btn-settings');
  await waitFor('!!document.querySelector(".drawer-settings.open")');
  await sleep(320); // Wait for the settings drawer's slide-in before hit-testing a touch.
  await evaluate('document.querySelector(".settings-font-range").scrollIntoView({block:"center"})');
  await send('Emulation.setTouchEmulationEnabled', { enabled: true });
  const touch = await evaluate(`(() => {const r=document.querySelector('.settings-font-range').getBoundingClientRect();return {x:r.left+r.width*.75,y:r.top+r.height/2,id:1};})()`);
  await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [touch] });
  await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  await waitFor('parseFloat(getComputedStyle(document.querySelector("#reader-column")).fontSize)>35');
  await select('[aria-label="阅读字号"]', '18.6');
  await waitFor('getComputedStyle(document.querySelector("#reader-column")).fontSize === "18.6px"');
  assert.equal(await evaluate('document.documentElement.scrollWidth'), 390);
  await shot('reader-settings-mobile.png');
  await nav(base + '/?debug=1', '!!window.__lib'); await click('#density-standard');
  assert.equal(await evaluate('document.documentElement.scrollWidth'), 390);
  await shot('shelf-mobile.png');
  await click(`.book-card[data-id="${text.id}"] .book-more`); await waitFor('!!document.querySelector("dialog[open]")');
  assert.ok(await evaluate('document.querySelector("dialog").getBoundingClientRect().right<=innerWidth'));
  await shot('book-details-mobile.png');
  await nav(base + `/?book=${comic.id}&debug=1`, '!!window.__comic');
  await click('.comic-tools [aria-label="阅读设置"]'); await waitFor('!!document.querySelector(".drawer-settings.open")');
  await select('[aria-label="漫画显示方式"]', 'width');
  assert.equal(await evaluate('window.__comic.fit()'), 'width');
  assert.deepEqual(errors, []);
  pass('390px 手机书架/详情/设置无横向溢出，漫画设置生效，全程无 JS 异常');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(300);
  fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}
