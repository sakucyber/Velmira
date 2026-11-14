// A disposable service and browser verify real IndexedDB + network loss + sync.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import assert from 'node:assert/strict';
import { once } from 'node:events';
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function port() { const s = net.createServer(); s.listen(0, '127.0.0.1'); await once(s, 'listening'); const p = s.address().port; await new Promise(r => s.close(r)); return p; }
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-offline-'));
const servicePort = await port(), debugPort = await port(), base = 'http://127.0.0.1:' + servicePort;
let service, browser, browser2, ws, seq = 0; const pending = new Map();
function send(method, params = {}) { const id = ++seq; return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); }); }
async function evaluate(expression) { const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }); assert.ok(!r.exceptionDetails, JSON.stringify(r.exceptionDetails)); return r.result.value; }
async function waitFor(expression) { for (let i = 0; i < 150; i++) { if (await evaluate(expression)) return; await sleep(100); } throw new Error('Timeout: ' + expression + '\n' + await evaluate('document.body.textContent.slice(0,3000)')); }
async function fetchJson(url, options) { const r = await fetch(base + url, options); if (!r.ok) throw new Error(await r.text()); return r.json(); }
try {
  service = spawn('python', ['-m', 'uvicorn', 'server.app.main:app', '--host', '127.0.0.1', '--port', String(servicePort), '--no-proxy-headers'], { cwd: path.resolve('.'), env: { ...process.env, DATA_DIR: path.join(temporary, 'data') }, stdio: 'ignore', windowsHide: true });
  for (let i = 0; i < 100; i++) { try { if ((await fetch(base + '/api/health')).ok) break; } catch {} await sleep(100); }
  const imported = await fetchJson('/api/books?filename=offline.txt', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: Buffer.from('测试正文。\n\n第二段可离线阅读。\n') });
  const bookId = imported.book.id;
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + debugPort, '--user-data-dir=' + path.join(temporary, 'browser'), 'about:blank'], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let i = 0; i < 100; i++) { try { target = (await (await fetch('http://127.0.0.1:' + debugPort + '/json')).json()).find(t => t.type === 'page'); if (target) break; } catch {} await sleep(100); }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const m = JSON.parse(event.data), p = pending.get(m.id); if (!p) return; pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); };
  await send('Page.enable'); await send('Network.enable'); await send('Page.navigate', { url: base + '/?book=' + bookId + '&debug=1' });
  await waitFor('[...document.querySelectorAll("#reader-column [data-i]")].some(p=>p.textContent.includes("测试正文"))');
  await evaluate(`(async()=>{ window.core=await import('/util/core.js'); window.ls=(await import('/util/local-store.js')).localStore; await ls.meta('auto-sync',false); await core.apiGet('/api/books'); await core.apiGet('/api/books/${bookId}/toc'); await core.apiGet('/api/books/${bookId}/bookmarks'); await core.apiGet('/api/books/${bookId}/annotations?limit=500'); return navigator.serviceWorker.ready.then(()=>true); })()`);
  await waitFor('!!navigator.serviceWorker.controller');
  const atomic = await evaluate(`(async()=>{
    const id='atomic-test', fields={book_id:'${bookId}',chapter:0,para:0,start_offset:0,end_offset:2,text:'测试',note:'保存成功的版本',sync_revision:0};
    await ls.commitLocalEdit('annotation','upsert',id,fields);
    const checks={}, errors=[];
    const snapshot=async()=>JSON.stringify({entity:(await ls.listAnnotations('${bookId}')).find(a=>a.id===id),pending:await ls.allPendingOps(),seq:await ls.meta('device-seq'),conflicts:await ls.meta('sync:conflicts'),id:ls.deviceId,persisted:await ls.meta('device-id'),localId:localStorage.getItem('velmira.device-id')});
    const failWrite=async(match,run)=>{
      const original=IDBObjectStore.prototype.put;
      IDBObjectStore.prototype.put=function(value,...args){if(match(this.name,value)){this.transaction.abort();throw new DOMException('Simulated full device storage','QuotaExceededError');}return original.call(this,value,...args);};
      try{await run();errors.push('not rejected');}catch(error){errors.push(typeof error.code==='string'?error.code:error.name);}finally{IDBObjectStore.prototype.put=original;}
    };
    const pending=(store,value)=>store==='ops'&&value.status==='pending';
    let before=await snapshot();
    await failWrite(pending,()=>ls.commitLocalEdit('annotation','upsert','atomic-create-failed',fields));
    checks.create=before===await snapshot()&&!(await ls.listAnnotations('${bookId}')).some(a=>a.id==='atomic-create-failed');
    await failWrite(pending,()=>core.apiPatch('/api/books/${bookId}/annotations/'+id,{note:'不能留下半条记录'}));
    checks.edit=before===await snapshot();
    await failWrite(pending,()=>ls.commitLocalEdit('annotation','delete',id,{book_id:'${bookId}'}));
    checks.delete=before===await snapshot();
    const operation=(await ls.allPendingOps()).find(op=>op.entityId===id), previousConflicts=await ls.meta('sync:conflicts')||[];
    await ls.meta('sync:conflicts',[...previousConflicts,{op_id:operation.opId,entity:'annotation',entity_id:id,reason:'revision_conflict',revision:4}]);
    before=await snapshot();
    await failWrite(pending,()=>ls.resolveConflict(operation.opId,'local'));
    checks.conflict=before===await snapshot();
    await failWrite((store,value)=>store==='meta'&&value.key==='sync:conflicts',()=>ls.resetDevice());
    checks.reset=before===await snapshot();
    await ls.acknowledge([operation.opId]);await ls.deleteAnnotation(id);await ls.meta('sync:conflicts',previousConflicts);
    return {checks,errors};
  })()`);
  assert.ok(Object.values(atomic.checks).every(Boolean), JSON.stringify(atomic));
  assert.deepEqual(atomic.errors, ['QuotaExceededError','local_storage_error','QuotaExceededError','QuotaExceededError','QuotaExceededError']);
  console.log('[PASS] quota abort rolls back create/edit/delete, conflict replacement and device migration without losing the draft');
  const tabs = await evaluate(`(async()=>{
    const iframe=document.createElement('iframe');iframe.hidden=true;document.body.appendChild(iframe);
    try{
      const other=(await iframe.contentWindow.eval('import('+JSON.stringify(location.origin+'/util/local-store.js')+')')).localStore;
      const operations=await Promise.all(Array.from({length:10},(_,i)=>[ls.enqueue('progress','upsert','tab-a-'+i,{chapter:0,para:0,offset:i}),other.enqueue('progress','upsert','tab-b-'+i,{chapter:0,para:0,offset:i})]).flat());
      const sequences=operations.map(op=>op.seq).sort((a,b)=>a-b),result={count:operations.length,unique:new Set(sequences).size,contiguous:sequences.every((seq,i)=>!i||seq===sequences[i-1]+1),identity:other.deviceId===ls.deviceId};
      await ls.acknowledge(operations.map(op=>op.opId));return result;
    }finally{iframe.remove();}
  })()`);
  assert.deepEqual(tabs,{count:20,unique:20,contiguous:true,identity:true});
  console.log('[PASS] simultaneous tabs allocate unique ordered operation sequences in IndexedDB');
  await send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  const offline = await evaluate(`(async()=>{
    const chapter=await core.apiGet('/api/books/${bookId}/chapters/0');
    const progress=await core.apiPut('/api/books/${bookId}/progress',{chapter:0,para:0,offset:2});
    const bookmark=await core.apiPost('/api/books/${bookId}/bookmarks',{chapter:0,para:0,offset:0});
    const annotation=await core.apiPost('/api/books/${bookId}/annotations',{chapter:0,para:0,start_offset:0,end_offset:2,text:'测试',note:'本机笔记',color:'yellow'});
    await core.apiPatch('/api/books/${bookId}/annotations/'+annotation.annotation.id,{note:'断网仍可修改'});
    const search=await core.apiGet('/api/books/${bookId}/search?q=测试');
    return {text:chapter.paragraphs[0],pending:(await ls.pendingOps()).length,note:(await core.apiGet('/api/books/${bookId}/annotations')).annotations[0].note,hits:search.hits.length,bookmarks:(await core.apiGet('/api/books/${bookId}/bookmarks')).bookmarks.length};
  })()`);
  assert.equal(offline.text, '测试正文。'); assert.ok(offline.pending >= 3); assert.equal(offline.note, '断网仍可修改'); assert.equal(offline.hits, 1); assert.equal(offline.bookmarks, 1);
  console.log('[PASS] IndexedDB caches, offline progress/bookmark/annotation CRUD and full-text search');
  await send('Page.reload');
  await waitFor('[...document.querySelectorAll("#reader-column [data-i]")].some(p=>p.textContent.includes("测试正文"))');
  await evaluate(`(async()=>{window.core=await import('/util/core.js');window.ls=(await import('/util/local-store.js')).localStore;return true;})()`);
  assert.equal(await evaluate(`(async()=> (await core.apiGet('/api/books/${bookId}/annotations')).annotations[0].note)()`), '断网仍可修改');
  console.log('[PASS] service worker shell and downloaded chapter survive an offline reload');
  await send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  const synced = await evaluate('(async()=>{const result=await ls.sync();return {result,pending:(await ls.pendingOps()).length};})()');
  assert.equal(synced.pending, 0, JSON.stringify(synced));
  const annotations = await fetchJson('/api/books/' + bookId + '/annotations');
  assert.equal(annotations.annotations[0].note, '断网仍可修改');
  assert.equal((await fetchJson('/api/books/' + bookId + '/bookmarks')).bookmarks.length, 1);
  assert.equal((await fetchJson('/api/books/' + bookId + '/progress')).progress.offset, 2);
  console.log('[PASS] reconnect push, pull, snapshot and acknowledgment reach the service');

  const firstSocket = ws, firstDevice = await evaluate('ls.deviceId'), secondPort = await port();
  browser2 = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + secondPort, '--user-data-dir=' + path.join(temporary, 'browser2'), 'about:blank'], { stdio: 'ignore', windowsHide: true });
  let secondTarget;
  for (let i = 0; i < 100; i++) { try { secondTarget = (await (await fetch('http://127.0.0.1:' + secondPort + '/json')).json()).find(t => t.type === 'page'); if (secondTarget) break; } catch {} await sleep(100); }
  assert.ok(secondTarget); ws = new WebSocket(secondTarget.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const m = JSON.parse(event.data), p = pending.get(m.id); if (!p) return; pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); };
  const secondSocket = ws;
  await send('Page.enable'); await send('Network.enable'); await send('Page.navigate', { url: base + '/?book=' + bookId + '&debug=1' });
  await waitFor('[...document.querySelectorAll("#reader-column [data-i]")].some(p=>p.textContent.includes("测试正文"))');
  await evaluate(`(async()=>{window.core=await import('/util/core.js');window.ls=(await import('/util/local-store.js')).localStore;await ls.meta('auto-sync',false);await ls.sync();return true;})()`);
  assert.notEqual(await evaluate('ls.deviceId'), firstDevice);
  const annotationId = annotations.annotations[0].id;
  assert.equal(await evaluate(`(async()=> (await ls.listAnnotations('${bookId}'))[0].note)()`), '断网仍可修改');
  ws = firstSocket;
  await send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await evaluate(`core.apiPatch('/api/books/${bookId}/annotations/${annotationId}',{note:'设备 A 离线版本'})`);
  ws = secondSocket;
  await evaluate(`(async()=>{await core.apiPatch('/api/books/${bookId}/annotations/${annotationId}',{note:'设备 B 先提交'});return ls.sync();})()`);
  ws = firstSocket;
  await send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  const conflict = await evaluate('(async()=>{const r=await ls.sync();return r.conflicts[0];})()');
  assert.equal(conflict.reason, 'revision_conflict');
  assert.equal(await evaluate(`(async()=> (await ls.listAnnotations('${bookId}'))[0].note)()`), '设备 A 离线版本');
  await evaluate(`(async()=>{await ls.resolveConflict('${conflict.op_id}','local');return ls.sync();})()`);
  assert.equal((await fetchJson('/api/books/' + bookId + '/annotations')).annotations[0].note, '设备 A 离线版本');
  ws = secondSocket;
  await evaluate('ls.sync()');
  assert.equal(await evaluate(`(async()=> (await ls.listAnnotations('${bookId}'))[0].note)()`), '设备 A 离线版本');
  console.log('[PASS] two independent devices retain a conflicting offline draft and converge after explicit resolution');
  secondSocket.close(); ws = firstSocket;
  await evaluate("ls.meta('auto-sync',true)");
  await send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await evaluate(`(async()=>{await core.apiPost('/api/books/${bookId}/bookmarks',{chapter:0,para:1,offset:0,label:'自动同步书签'});await core.apiPut('/api/books/${bookId}/progress',{chapter:0,para:0,offset:3});return true;})()`);
  assert.ok(await evaluate('(async()=> (await ls.allPendingOps()).length>=2)()'));
  await send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await waitFor('(async()=> (await ls.allPendingOps()).length===0)()');
  assert.ok((await fetchJson('/api/books/'+bookId+'/bookmarks')).bookmarks.some(bookmark=>bookmark.label==='自动同步书签'));
  assert.equal((await fetchJson('/api/books/'+bookId+'/progress')).progress.offset,3);
  console.log('[PASS] reconnect event automatically synchronizes offline bookmarks and progress without a manual sync');
} finally {
  ws?.close();
  for (const child of [browser2, browser, service]) if (child && child.exitCode === null) {
    const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' }); await once(killer, 'exit');
  }
  await sleep(300); await fs.promises.rm(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
