// End-to-end checks against an installed debuggable APK. Never clears app data.
// ANDROID_SERIAL defaults to the user's emulator; ANDROID_ADB may override adb.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const serial = process.env.ANDROID_SERIAL || '127.0.0.1:16384';
const adb = process.env.ANDROID_ADB || 'C:/Users/saku_cyber/.workbuddy/binaries/platform-tools/platform-tools/adb.exe';
const sleep = ms => new Promise(r => setTimeout(r, ms));
const adbRun = args => execFileSync(adb, ['-s', serial, ...args], { encoding: 'utf8', windowsHide: true });
let ws, sequence = 0; const pending = new Map(), errors = [];
async function connect() {
  const pid = adbRun(['shell', 'pidof', 'dev.velmira.mobile']).trim(); assert.ok(pid);
  try { adbRun(['forward', '--remove', 'tcp:9222']); } catch {}
  adbRun(['forward', 'tcp:9222', 'localabstract:webview_devtools_remote_' + pid]);
  let targets;
  for (let i = 0; i < 50; i++) { try { targets = await (await fetch('http://127.0.0.1:9222/json')).json(); if (targets.some(t => t.type === 'page')) break; } catch {} await sleep(100); }
  const target = targets.find(t => t.type === 'page' && JSON.parse(t.description || '{}').visible) || targets.find(t => t.type === 'page');
  assert.ok(target, 'No Android WebView target');
  ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  ws.onmessage = event => {
    const msg = JSON.parse(event.data), item = pending.get(msg.id);
    if (item) { pending.delete(msg.id); msg.error ? item.reject(msg.error) : item.resolve(msg.result); }
    else if (msg.method === 'Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.text);
  };
  await send('Runtime.enable'); await send('Page.enable');
}
function send(method, params = {}) { return new Promise((resolve, reject) => { const id = ++sequence; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); }); }
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result?.value;
}
async function waitFor(expression) {
  for (let i = 0; i < 150; i++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timed out: ' + expression + '\n' + await evaluate('document.body.innerText.slice(0,1800)'));
}
async function api(url, method = 'GET', body) {
  const result = await evaluate(`(async()=>{const r=await fetch(${JSON.stringify(url)},{method:${JSON.stringify(method)},headers:{'Content-Type':'application/json'},${body === undefined ? '' : 'body:JSON.stringify(' + JSON.stringify(body) + '),'}});return {status:r.status,body:await r.json()};})()`);
  assert.ok(result.status >= 200 && result.status < 300, JSON.stringify(result)); return result.body;
}
async function navigate(url) { await send('Page.navigate', { url }); await waitFor('document.readyState==="complete"'); }
async function screenshot(name) {
  try { const shot = await send('Page.captureScreenshot', { format: 'png' }); fs.mkdirSync('tests/screenshots', { recursive: true }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(shot.data, 'base64')); } catch (e) { console.log('Screenshot unavailable:', e.message); }
}
function crc32(bytes) { let crc = 0xffffffff; for (const b of bytes) { crc ^= b; for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1)); } return (crc ^ 0xffffffff) >>> 0; }
function zip(files) {
  const local = [], central = []; let offset = 0;
  for (const [name, raw] of files) {
    const filename = Buffer.from(name), data = Buffer.isBuffer(raw) ? raw : Buffer.from(raw), crc = crc32(data);
    const head = Buffer.alloc(30); head.writeUInt32LE(0x04034b50); head.writeUInt16LE(20, 4); head.writeUInt32LE(crc, 14); head.writeUInt32LE(data.length, 18); head.writeUInt32LE(data.length, 22); head.writeUInt16LE(filename.length, 26);
    local.push(head, filename, data);
    const c = Buffer.alloc(46); c.writeUInt32LE(0x02014b50); c.writeUInt16LE(20, 4); c.writeUInt16LE(20, 6); c.writeUInt32LE(crc, 16); c.writeUInt32LE(data.length, 20); c.writeUInt32LE(data.length, 24); c.writeUInt16LE(filename.length, 28); c.writeUInt32LE(offset, 42); central.push(c, filename);
    offset += head.length + filename.length + data.length;
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50); end.writeUInt16LE(files.length, 8); end.writeUInt16LE(files.length, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, directory, end]);
}
async function upload(bytes, name) {
  return evaluate(`(async()=>{const body=Uint8Array.from(atob(${JSON.stringify(bytes.toString('base64'))}),c=>c.charCodeAt(0));const r=await fetch('/api/books?filename='+encodeURIComponent(${JSON.stringify(name)}),{method:'POST',headers:{'Content-Type':'application/octet-stream'},body});return {status:r.status,body:await r.json()};})()`);
}
try {
  await connect(); await waitFor('!!document.querySelector(".onboarding") || !!document.querySelector("#library-view:not([hidden])")');
  const origin = await evaluate('location.origin');
  assert.equal((await api('/api/health')).standalone, true);
  assert.equal((await api('/api/auth/status')).standalone, true);
  if (await evaluate('!!document.querySelector(".onboarding")')) {
    assert.equal(await evaluate('document.querySelectorAll(".onboarding input:not([type=range])").length'), 0);
    await screenshot('android-standalone-onboarding');
    await evaluate('[...document.querySelectorAll(".onboarding button")].find(x=>x.textContent==="跳过，直接进入").click()');
  }
  await waitFor('!!document.querySelector("#library-view:not([hidden])")');
  const txt = '第一章 本机阅读\n\n甲😀乙在窗边翻开书页。\n\n这是一份本机离线验收文件，阅读进度应保存。\n\n第二章 再次打开\n\n关闭应用后，书籍与笔记仍在这里。\n';
  // Use the actual input change/XHR path, including navigation into the reader.
  await evaluate(`(()=>{const f=new File([${JSON.stringify(txt)}],'Velmira-Android-验收.txt',{type:'text/plain'}),d=new DataTransfer();d.items.add(f);const n=document.querySelector('#file-input');n.files=d.files;n.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  await waitFor('location.search.includes("book=") && document.querySelector("#book-title")?.textContent.includes("Velmira-Android") && !document.querySelector(".fatal")');
  const txtId = await evaluate('new URLSearchParams(location.search).get("book")'); assert.ok(txtId);
  assert.ok((await api('/api/books')).books.some(b => b.id === txtId), 'Imported book appears on the active shelf');
  await api('/api/books/' + txtId, 'PATCH', { archived: true });
  assert.ok(!(await api('/api/books')).books.some(b => b.id === txtId));
  assert.ok((await api('/api/books?archived=true')).books.some(b => b.id === txtId));
  await api('/api/books/' + txtId, 'PATCH', { archived: false });
  assert.ok((await api('/api/books')).books.some(b => b.id === txtId));
  await navigate(origin + '/?book=' + txtId + '&debug=1'); await waitFor('!!window.__velmira && !!document.querySelector("#reader-column p")');
  assert.ok(await evaluate('document.querySelector("#reader-column").textContent.includes("甲😀乙")'));
  await screenshot('android-standalone-txt-reader');
  const chapter = await api('/api/books/' + txtId + '/chapters/0'); assert.equal(chapter.paragraphs[0], '甲😀乙在窗边翻开书页。');
  await api('/api/books/' + txtId + '/progress', 'PUT', { chapter: 1, para: 0, offset: 2 });
  const bookmark = (await api('/api/books/' + txtId + '/bookmarks', 'POST', { chapter: 0, para: 0, offset: 1, label: '安卓书签' })).bookmark;
  const annotation = (await api('/api/books/' + txtId + '/annotations', 'POST', { chapter: 0, para: 0, start_offset: 0, end_offset: 3, text: '甲😀乙', note: '安卓本机笔记', color: 'yellow' })).annotation;
  await api('/api/books/' + txtId + '/annotations/' + annotation.id, 'PATCH', { note: '重启后仍应保留', color: 'green' });
  const hits = await api('/api/books/' + txtId + '/search?q=' + encodeURIComponent('书页')); assert.ok(hits.hits.length > 0);
  console.log('[PASS] Android WebView file input/XHR imports TXT, renders Unicode text, saves progress/bookmark/annotation and searches');
  const png = fs.readFileSync('web/icons/icon-192.png');
  const makeEpub = (content, title = 'Android EPUB 插图验收') => zip([
    ['mimetype', 'application/epub+zip'], ['META-INF/container.xml', '<container><rootfiles><rootfile full-path="OEBPS/book.opf"/></rootfiles></container>'],
    ['OEBPS/book.opf', '<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>' + title + '</dc:title></metadata><manifest><item id="ch" href="one.xhtml"/></manifest><spine><itemref idref="ch"/></spine></package>'],
    ['OEBPS/one.xhtml', content], ['OEBPS/image.png', png]
  ]);
  const epub = makeEpub('<html><body><h1>本机插图</h1><p>EPUB 插图应在离线书库中显示。</p><img src="image.png"/></body></html>');
  const epubRes = await upload(epub, 'Velmira-Android-验收.epub'); assert.ok(epubRes.status < 300, JSON.stringify(epubRes)); const epubId = epubRes.body.book.id;
  const epubCh = await api('/api/books/' + epubId + '/chapters/0'); assert.ok(epubCh.illustrations.length);
  await navigate(origin + '/?book=' + epubId + '&debug=1');
  await waitFor('!!window.__velmira && [...document.querySelectorAll("#reader-main img")].some(x=>x.complete && x.naturalWidth>0)');
  await screenshot('android-standalone-epub-reader');
  const cbzRes = await upload(zip([['pages/1.png', png], ['pages/2.png', png]]), 'Velmira-Android-验收.cbz'); assert.ok(cbzRes.status < 300, JSON.stringify(cbzRes)); const cbzId = cbzRes.body.book.id;
  await navigate(origin + '/?book=' + cbzId + '&debug=1');
  await waitFor('!!window.__comic && document.querySelector(".comic-img")?.naturalWidth>0');
  await evaluate('window.__comic.go(1)'); await waitFor('window.__comic.page()===1'); await screenshot('android-standalone-cbz-reader');
  console.log('[PASS] Android EPUB illustration and CBZ images decode from the app-private local service');
  const invalidArchive = await upload(zip([['../escape.png', png]]), 'invalid.cbz'); assert.ok(invalidArchive.status >= 400);
  const invalidXml = await upload(makeEpub('<!DOCTYPE html [<!ENTITY xxe SYSTEM "file:///system/build.prop">]><html><body><p>&xxe;</p></body></html>'), 'invalid.epub'); assert.ok(invalidXml.status >= 400);
  const imageOnly = await upload(makeEpub('<html><body><img src="image.png"/></body></html>', 'Android 纯图片 EPUB 验收'), 'image-only.epub'); assert.ok(imageOnly.status < 300, JSON.stringify(imageOnly));
  await api('/api/books/' + imageOnly.body.book.id + '/progress', 'PUT', { chapter: 0, para: 0, offset: 0 });
  await api('/api/books/' + imageOnly.body.book.id, 'PATCH', { archived: true });
  console.log('[PASS] Shelf archive/restore works; hostile ZIP/XML imports reject; image-only EPUB progress saves');
  // A real restart removes the old session and requires a fresh authenticated WebView.
  await navigate(origin + '/'); await waitFor('!!document.querySelector("#library-view:not([hidden])")');
  await api('/api/books/' + txtId + '/progress', 'PUT', { chapter: 1, para: 0, offset: 2 });
  ws.close(); adbRun(['shell', 'am', 'force-stop', 'dev.velmira.mobile']); adbRun(['shell', 'am', 'start', '-n', 'dev.velmira.mobile/.MainActivity']); await sleep(1200); await connect();
  await waitFor('!!document.querySelector("#library-view:not([hidden])")'); assert.equal(await evaluate('location.origin'), origin);
  assert.equal(await evaluate('!!document.querySelector(".onboarding")'), false);
  const books = (await api('/api/books')).books; for (const id of [txtId, epubId, cbzId]) assert.ok(books.some(b => b.id === id));
  assert.equal((await api('/api/books/' + txtId + '/progress')).progress.chapter, 1);
  assert.ok((await api('/api/books/' + txtId + '/bookmarks')).bookmarks.some(b => b.id === bookmark.id));
  assert.ok((await api('/api/books/' + txtId + '/annotations')).annotations.some(a => a.id === annotation.id && a.note === '重启后仍应保留'));
  // Relaunch the icon a second time: singleTask must preserve the same server/origin.
  adbRun(['shell', 'am', 'start', '-n', 'dev.velmira.mobile/.MainActivity']); await sleep(300);
  const pages = (await (await fetch('http://127.0.0.1:9222/json')).json()).filter(t => t.type === 'page'); assert.equal(pages.length, 1);
  assert.equal(await evaluate('location.origin'), origin); await screenshot('android-standalone-persisted-library');
  assert.deepEqual(errors, []);
  console.log('[PASS] Force-stop/restart preserves books, progress, bookmark, annotation and first-run completion; repeated launch retains one WebView/origin');
  console.log(JSON.stringify({ serial, origin, txtId, epubId, cbzId, books: books.length }));
} finally { ws?.close(); }
