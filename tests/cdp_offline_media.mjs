// Real CBZ/EPUB downloads, offline reloads and device-only removal.
// Each run owns its service, data directory, browser and debugging port.
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function freePort() {
  const socket = net.createServer(); socket.listen(0, '127.0.0.1');
  await once(socket, 'listening'); const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve)); return port;
}
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-offline-media-'));
const servicePort = await freePort(), debugPort = await freePort();
const base = 'http://127.0.0.1:' + servicePort;
const pending = new Map(), errors = [];
let service, browser, ws, sequence = 0;
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
}
async function waitFor(expression) {
  for (let index = 0; index < 180; index++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression + '\n' + await evaluate('document.body.textContent.slice(0,2000)'));
}
async function api(url, options) {
  const response = await fetch(base + url, options);
  if (!response.ok) throw new Error(url + ': ' + response.status + ' ' + await response.text());
  return response.json();
}
async function navigate(url, ready) { await send('Page.navigate', { url }); await waitFor(ready); }
async function network(offline) {
  await send('Network.emulateNetworkConditions', { offline, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
}
const comicReady = '!!document.querySelector(".comic-img") && document.querySelector(".comic-img").complete && document.querySelector(".comic-img").naturalWidth > 0 && document.querySelector(".comic-img").src.startsWith("blob:")';
const epubReady = '!!document.querySelector(".epub-illustrations img") && document.querySelector(".epub-illustrations img").complete && document.querySelector(".epub-illustrations img").naturalWidth > 0 && document.querySelector("#reader-column").textContent.includes("离线插图测试正文")';
async function countImages(bookId) {
  return evaluate(`(async()=>{const db=await new Promise((resolve,reject)=>{const request=indexedDB.open('velmira-media',1);request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});try{return await new Promise((resolve,reject)=>{const request=db.transaction('images').objectStore('images').index('bookId').count(${JSON.stringify(bookId)});request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});}finally{db.close();}})()`);
}
async function screenshot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const result = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(result.data, 'base64'));
}

try {
  const epubPath = path.join(temporary, 'illustrated.epub');
  const pngPath = path.resolve('web/icons/icon-192.png');
  const fixture = spawnSync('python', ['-c', `
import pathlib, sys, zipfile
with zipfile.ZipFile(sys.argv[1], 'w') as archive:
    archive.writestr('mimetype', 'application/epub+zip')
    archive.writestr('META-INF/container.xml', '''<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>''')
    archive.writestr('OEBPS/book.opf', '''<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="book-id">offline-media-fixture</dc:identifier><dc:title>离线插图示例</dc:title><dc:language>zh-CN</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="picture" href="images/picture.png" media-type="image/png"/></manifest><spine><itemref idref="chapter"/></spine></package>''')
    archive.writestr('OEBPS/chapter.xhtml', '''<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>温室</title></head><body><h1>第一章 温室</h1><p>离线插图测试正文，林青走进温室。</p><p>这段正文与图片一起保存在本设备。</p><img src="images/picture.png" alt="温室插图"/></body></html>''')
    archive.writestr('OEBPS/images/picture.png', pathlib.Path(sys.argv[2]).read_bytes())
`, epubPath, pngPath], { encoding: 'utf8', windowsHide: true });
  assert.equal(fixture.status, 0, fixture.stderr);
  service = spawn('python', ['-m', 'uvicorn', 'server.app.main:app', '--host', '127.0.0.1', '--port', String(servicePort), '--no-proxy-headers'], {
    cwd: path.resolve('.'), env: { ...process.env, DATA_DIR: path.join(temporary, 'data') }, stdio: 'ignore', windowsHide: true,
  });
  let ready = false;
  for (let index = 0; index < 100; index++) {
    try { if ((await fetch(base + '/api/health')).ok) { ready = true; break; } } catch {}
    if (service.exitCode !== null) break; await sleep(100);
  }
  assert.ok(ready, 'Disposable service starts');
  const upload = async (filename, body) => (await api('/api/books?filename=' + encodeURIComponent(filename), { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body })).book;
  const comic = await upload('offline-comic.cbz', fs.readFileSync('samples/demo-comic.cbz'));
  const epub = await upload('illustrated.epub', fs.readFileSync(epubPath));
  assert.equal(comic.format, 'cbz'); assert.ok(comic.chapter_count > 1); assert.equal(epub.format, 'epub');
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', [
    '--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + debugPort,
    '--user-data-dir=' + path.join(temporary, 'browser'), '--window-size=1000,850', 'about:blank',
  ], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let index = 0; index < 100; index++) {
    try { target = (await (await fetch('http://127.0.0.1:' + debugPort + '/json')).json()).find(item => item.type === 'page'); if (target) break; } catch {}
    await sleep(100);
  }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) { const request = pending.get(message.id); pending.delete(message.id); message.error ? request.reject(message.error) : request.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
  };
  await send('Page.enable'); await send('Runtime.enable'); await send('Network.enable');
  await send('Network.setCacheDisabled', { cacheDisabled: true });
  await navigate(base + '/?debug=1', '!!window.__lib');
  const downloads = await evaluate(`(async()=>{
    const {localStore}=await import('/util/local-store.js'),{downloadBook}=await import('/ui/offline-settings.js');
    await localStore.meta('auto-sync',false);
    await downloadBook(${JSON.stringify(comic)},()=>{},new AbortController().signal);
    await downloadBook(${JSON.stringify(epub)},()=>{},new AbortController().signal);
    await navigator.serviceWorker.ready;
    return [await localStore.meta(${JSON.stringify('download:' + comic.id)}),await localStore.meta(${JSON.stringify('download:' + epub.id)})];
  })()`);
  assert.equal(downloads[0].complete, true); assert.equal(downloads[0].count, comic.chapter_count);
  assert.equal(downloads[1].complete, true); assert.equal(downloads[1].count, epub.chapter_count);
  assert.equal(await countImages(comic.id), comic.chapter_count); assert.equal(await countImages(epub.id), 1);
  await waitFor('!!navigator.serviceWorker.controller');
  console.log('[PASS] Real CBZ and illustrated EPUB download into durable chapter/page/blob stores');

  await navigate(base + '/?book=' + comic.id + '&ch=0&debug=1', comicReady);
  await network(true); await send('Page.reload', { ignoreCache: true }); await waitFor(comicReady);
  assert.equal(await evaluate('window.__comic.page()'), 0);
  const firstSrc = await evaluate('document.querySelector(".comic-img").src');
  await evaluate('document.querySelector(".comic-next").click()');
  await waitFor('window.__comic.page()===1 && ' + comicReady);
  assert.notEqual(await evaluate('document.querySelector(".comic-img").src'), firstSrc);
  await evaluate('document.querySelector(".comic-prev").click()'); await waitFor('window.__comic.page()===0 && ' + comicReady);
  await screenshot('offline-cbz-reloaded');
  console.log('[PASS] Offline CBZ reload and next/previous pages decode Blob images with HTTP cache disabled');

  await navigate(base + '/?book=' + epub.id + '&ch=0&debug=1', epubReady);
  await send('Page.reload', { ignoreCache: true }); await waitFor(epubReady);
  const picture = await evaluate(`(async()=>{const image=document.querySelector('.epub-illustrations img');const response=await fetch(image.src);const bytes=new Uint8Array(await response.arrayBuffer());return {src:image.src,width:image.naturalWidth,length:bytes.length,signature:[...bytes.slice(0,8)],text:document.querySelector('#reader-column').textContent};})()`);
  assert.ok(picture.src.startsWith('blob:')); assert.equal(picture.width, 192);
  assert.equal(picture.length, fs.statSync(pngPath).size); assert.deepEqual(picture.signature, [137, 80, 78, 71, 13, 10, 26, 10]);
  assert.ok(picture.text.includes('这段正文与图片一起保存在本设备'));
  await screenshot('offline-epub-reloaded');
  console.log('[PASS] Offline EPUB reload renders body and decodable illustration; Blob bytes match the PNG fixture');

  await network(false); await navigate(base + '/?debug=1', '!!window.__lib');
  await evaluate(`(async()=>{const {localStore}=await import('/util/local-store.js');await localStore.sync();return true;})()`);
  await evaluate('document.querySelector("#rail-settings").click()');
  await waitFor('!!document.querySelector(".settings-tabs [data-section=offline]")');
  await evaluate('document.querySelector(".settings-tabs [data-section=offline]").click()');
  await waitFor('document.querySelectorAll(".offline-book").length===2');
  for (const book of [comic, epub]) {
    const selector = '[aria-label=' + JSON.stringify('清除下载 ' + book.title) + ']';
    await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    await waitFor('!!document.querySelector(".offline-remove-confirm")');
    await evaluate('[...document.querySelectorAll(".offline-remove-confirm button")].find(button=>button.textContent==="确认清除").click()');
    await waitFor('!document.querySelector(".offline-remove-confirm")');
    assert.equal(await countImages(book.id), 0);
    const marker = await evaluate(`(async()=>{const {localStore}=await import('/util/local-store.js');return (await localStore.meta(${JSON.stringify('download:' + book.id)}))??null;})()`);
    assert.equal(marker, null); assert.equal((await api('/api/books/' + book.id)).book.id, book.id);
  }
  assert.equal((await api('/api/books')).books.length, 2);
  assert.deepEqual(errors, []);
  console.log('[PASS] Confirmed device-only removal clears both books’ image Blobs/download markers and preserves server books; zero JS exceptions');
} finally {
  ws?.close();
  for (const child of [browser, service]) if (child && child.exitCode === null) {
    if (process.platform === 'win32') { const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true }); await once(killer, 'exit'); }
    else { child.kill(); await once(child, 'exit'); }
  }
  const target = path.resolve(temporary), parent = path.resolve(os.tmpdir()) + path.sep;
  assert.ok(target.startsWith(parent) && path.basename(target).startsWith('velmira-offline-media-'), 'Cleanup stays inside the owned temporary directory');
  await fs.promises.rm(target, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
