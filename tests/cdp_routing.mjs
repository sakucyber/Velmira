// Disposable UI regression for model pools. Uses only localhost connections and no generation calls.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8894';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-routing-'));
const cdpPort = 9346, pending = new Map(), errors = [];
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0, bookId;

async function api(url, method = 'GET', data) {
  const response = await fetch(base + url, { method, body: data && JSON.stringify(data), headers: data ? { 'Content-Type': 'application/json' } : undefined });
  const body = await response.text();
  assert.ok(response.ok, `${method} ${url}: ${response.status} ${body}`);
  return body ? JSON.parse(body) : null;
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
async function waitFor(expression, limit = 180) {
  for (let i = 0; i < limit; i++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression);
}
async function clickText(text, root = '.drawer.open') {
  const expression = `(() => { const root=document.querySelector(${JSON.stringify(root)}); return !!root && [...root.querySelectorAll('button')].some(b=>b.textContent.trim()===${JSON.stringify(text)}&&!b.disabled&&b.getClientRects().length); })()`;
  await waitFor(expression);
  await evaluate(`(() => { const root=document.querySelector(${JSON.stringify(root)}); const b=[...root.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}&&!b.disabled); b.scrollIntoView({block:'center'}); b.click(); })()`);
}
async function shot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const result = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(result.data, 'base64'));
}
async function poolEval(kind, body) {
  return evaluate(`(() => { const pool=document.querySelector('.model-routing[data-routing=${JSON.stringify(kind)}]'); if(!pool) throw new Error(${JSON.stringify('Missing ' + kind + ' model pool')}); ${body} })()`);
}
async function poolClick(kind, text) {
  await poolEval(kind, `const b=[...pool.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}&&!b.disabled); if(!b) throw new Error('Missing pool button '+${JSON.stringify(text)}); b.click();`);
}
async function poolAdd(kind, model) {
  await poolEval(kind, `const select=pool.querySelector('.routing-add select'); const option=[...select.options].find(o=>o.textContent.includes(${JSON.stringify(model)})); if(!option) throw new Error('Missing fixture model'); select.value=option.value; select.dispatchEvent(new Event('change',{bubbles:true})); const b=[...pool.querySelectorAll('.routing-add button')].find(b=>b.textContent.trim()==='添加模型'); if(!b) throw new Error('Missing add model button'); b.click();`);
  await sleep(80);
}
try {
  await api('/api/ai/settings', 'PATCH', { enabled: false, auto_analyze: false });
  for (const kind of ['text', 'image']) {
    const initial = (await api('/api/ai/routing/' + kind)).config;
    await api('/api/ai/routing/' + kind, 'PATCH', { ...initial, enabled: false, models: [], strategy: 'ordered' });
  }
  const fixture = '第一章 模型池回归\n\n这是只用于界面测试的原创正文，不触发文本分析或图像生成。';
  const upload = await fetch(base + '/api/books?filename=模型池回归.txt', { method: 'POST', body: fixture, headers: { 'Content-Type': 'application/octet-stream' } });
  const uploaded = await upload.json();
  assert.ok(upload.ok, JSON.stringify(uploaded));
  bookId = uploaded.book.id;
  const prefix = '/api/books/' + bookId + '/manga';
  await api(prefix + '/config', 'PATCH', { provider: 'mock', width: 256, height: 256 });
  const suffix = createHash('sha1').update(String(Date.now())).digest('hex').slice(0, 8);
  const fixtureModels = { text: ['route-text-a-' + suffix, 'route-text-b-' + suffix], image: ['route-image-a-' + suffix, 'route-image-b-' + suffix] };
  for (const [kind, names] of Object.entries(fixtureModels)) {
    for (const model of names) {
      await api('/api/ai/connections', 'POST', { label: model, kind, base_url: 'http://127.0.0.1:1/v1', models: [model], api_key: 'local-fixture-key', rpm: 25, max_concurrency: 2, json_mode: true, reference_api: 'generations' });
    }
  }

  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + cdpPort, '--user-data-dir=' + profile, '--window-size=1280,900', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${cdpPort}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target);
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const message = JSON.parse(event.data); if (pending.has(message.id)) { const p = pending.get(message.id); pending.delete(message.id); message.error ? p.reject(message.error) : p.resolve(message.result); } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + bookId + '&debug=1' });
  await waitFor('!!window.__velmira && !!document.querySelector("#btn-ai") && !document.querySelector("#btn-ai").disabled');
  await evaluate('document.querySelector("#btn-ai").click()');
  await waitFor('!!document.querySelector(".drawer-ai.open")');
  await clickText('漫画生成');
  await waitFor('!!document.querySelector(".manga-tabs")');
  await clickText('漫画与画风设置');
  await waitFor('!!document.querySelector(".drawer-settings.open .model-routing[data-routing=text] .routing-add select") && !!document.querySelector(".drawer-settings.open .model-routing[data-routing=image] .routing-add select")');

  const textPool = '.model-routing[data-routing="text"]';
  const imagePool = '.model-routing[data-routing="image"]';
  assert.equal(await evaluate(`getComputedStyle(document.querySelector(${JSON.stringify(textPool)}).querySelector('.routing-models')).listStyleType`), 'none');
  await evaluate('document.querySelector("#debug-badge")?.remove()');
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelectorAll('.routing-model').length`), 0);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(imagePool)}).querySelectorAll('.routing-model').length`), 0);
  await poolAdd('text', fixtureModels.text[0]); await poolAdd('text', fixtureModels.text[1]);
  async function dragWithMouse(from, to) {
    const points = await poolEval('text', `const rows=[...pool.querySelectorAll('.routing-model')]; rows[${from}].scrollIntoView({block:'center'}); const a=rows[${from}].querySelector('.routing-drag-handle').getBoundingClientRect(),b=rows[${to}].getBoundingClientRect();return {start:{x:a.left+a.width/2,y:a.top+a.height/2},end:{x:a.left+a.width/2,y:b.top+b.height*.85}};`);
    await send('Input.dispatchMouseEvent', { type: 'mousePressed', ...points.start, button: 'left', buttons: 1, clickCount: 1 });
    await sleep(40);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...points.end, button: 'left', buttons: 1 });
    await sleep(40);
    await send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...points.end, button: 'left', buttons: 0, clickCount: 1 });
  }
  await dragWithMouse(0, 1);
  assert.equal(await poolEval('text', `return pool.querySelector('.routing-model').dataset.model;`), fixtureModels.text[1]);
  assert.equal((await api('/api/ai/routing/text')).config.models.length, 0, 'Dragging must remain unsaved');
  await dragWithMouse(0, 1);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelectorAll('.routing-model strong')[0].textContent.includes('route-text-a')`), true);
  await poolEval('text', `const b=[...pool.querySelectorAll('.routing-model')][1].querySelector('button[aria-label*="上移"]'); b.click();`);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelectorAll('.routing-model strong')[0].textContent.includes('route-text-b')`), true);
  await poolEval('text', `const b=[...pool.querySelectorAll('.routing-model')][0].querySelector('button[aria-label*="移除"]'); b.click();`);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelectorAll('.routing-model').length`), 1);
  await poolAdd('text', fixtureModels.text[1]);
  await poolClick('text', '轮流');
  await poolEval('text', `pool.querySelector('.routing-enable input').checked=true; pool.querySelector('.routing-enable input').dispatchEvent(new Event('input',{bubbles:true}));`);
  await poolEval('text', `const fields=[...pool.querySelector('.routing-advanced .routing-parameters').querySelectorAll('input[type=number]')]; fields[0].value='0.4'; fields[0].dispatchEvent(new Event('input',{bubbles:true})); fields[1].value='4096'; fields[1].dispatchEvent(new Event('input',{bubbles:true})); fields[2].value='45'; fields[2].dispatchEvent(new Event('input',{bubbles:true})); fields[3].value='30'; fields[3].dispatchEvent(new Event('input',{bubbles:true}));`);
  await poolClick('text', '立即保存');
  await waitFor(`document.querySelector(${JSON.stringify(textPool)}).textContent.includes('已自动保存')`);
  let textRouting = await api('/api/ai/routing/text');
  assert.equal(textRouting.config.enabled, true); assert.equal(textRouting.config.strategy, 'round_robin'); assert.equal(textRouting.config.models.length, 2);
  assert.equal(textRouting.config.temperature, 0.4); assert.equal(textRouting.config.max_tokens, 4096); assert.equal(textRouting.config.timeout, 45); assert.equal(textRouting.config.cooldown, 30);
  await poolClick('text', '按顺序'); await poolClick('text', '立即保存');
  await waitFor(`!document.querySelector(${JSON.stringify(textPool)}).hasAttribute('aria-busy')`);
  assert.equal((await api('/api/ai/routing/text')).config.strategy, 'ordered');
  await poolClick('text', '轮流'); await poolClick('text', '立即保存');
  await waitFor(`!document.querySelector(${JSON.stringify(textPool)}).hasAttribute('aria-busy')`);
  textRouting = await api('/api/ai/routing/text');

  await poolAdd('image', fixtureModels.image[0]); await poolAdd('image', fixtureModels.image[1]);
  await poolEval('image', `const b=[...pool.querySelectorAll('.routing-model')][1].querySelector('button[aria-label*="上移"]'); b.click();`);
  await poolEval('image', `const b=[...pool.querySelectorAll('.routing-model')][0].querySelector('button[aria-label*="下移"]'); b.click();`);
  await poolEval('image', `const b=[...pool.querySelectorAll('.routing-model')][0].querySelector('button[aria-label*="移除"]'); b.click();`);
  await poolClick('image', '随机');
  await poolEval('image', `pool.querySelector('.routing-enable input').checked=true; pool.querySelector('.routing-enable input').dispatchEvent(new Event('input',{bubbles:true}));`);
  await poolEval('image', `const fields=[...pool.querySelector('.routing-advanced .routing-parameters').querySelectorAll('input[type=number]')]; fields[0].value='90'; fields[0].dispatchEvent(new Event('input',{bubbles:true})); fields[1].value='20'; fields[1].dispatchEvent(new Event('input',{bubbles:true}));`);
  await poolClick('image', '立即保存');
  await waitFor(`document.querySelector(${JSON.stringify(imagePool)}).textContent.includes('已自动保存')`);
  const imageRouting = await api('/api/ai/routing/image');
  assert.equal(imageRouting.config.enabled, true); assert.equal(imageRouting.config.strategy, 'random'); assert.equal(imageRouting.config.models.length, 1);
  assert.equal(imageRouting.config.timeout, 90); assert.equal(imageRouting.config.cooldown, 20);
  await poolClick('text', '刷新文本路由状态'); await poolClick('image', '刷新生图路由状态');
  assert.equal((await api('/api/ai/routing/text')).status.attempts, 0);
  assert.equal((await api('/api/ai/routing/image')).status.attempts, 0);
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await poolEval('text', `pool.scrollIntoView({block:'start'});`);
  await sleep(150);
  await shot('routing-desktop');

  // Reader canonicalizes its URL without debug; restore the flag before a real reload.
  await evaluate('(() => { const url=new URL(location.href); url.searchParams.set("debug","1"); history.replaceState(null,"",url); })()');
  await send('Page.reload', { ignoreCache: true });
  await waitFor('!!window.__velmira && !!document.querySelector("#btn-ai") && !document.querySelector("#btn-ai").disabled');
  await evaluate('document.querySelector("#btn-ai").click()'); await waitFor('!!document.querySelector(".drawer-ai.open")'); await clickText('漫画生成'); await clickText('漫画与画风设置');
  await waitFor(`!!document.querySelector(${JSON.stringify(textPool + ' .routing-model')}) && !!document.querySelector(${JSON.stringify(imagePool + ' .routing-model')})`);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelectorAll('.routing-model').length`), 2);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(imagePool)}).querySelectorAll('.routing-model').length`), 1);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).querySelector('.routing-enable input').checked`), true);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(imagePool)}).querySelector('.routing-enable input').checked`), true);
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await waitFor('document.documentElement.scrollWidth<=innerWidth');
  // The drawer still transitions from the desktop X slide to the mobile sheet.
  await sleep(400);
  assert.equal(await evaluate('document.querySelector(".drawer-settings").getBoundingClientRect().right<=innerWidth'), true);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(textPool)}).scrollWidth<=document.querySelector(${JSON.stringify(textPool)}).clientWidth+1`), true);
  assert.equal(await evaluate(`document.querySelector(${JSON.stringify(imagePool)}).scrollWidth<=document.querySelector(${JSON.stringify(imagePool)}).clientWidth+1`), true);
  await evaluate('document.querySelector("#debug-badge")?.remove()');
  await poolEval('text', `pool.scrollIntoView({block:'start'});`);
  await sleep(150);
  await shot('routing-mobile');
  // CDP emits actual touch events, which the browser turns into Pointer Events.
  const touchPoints = await poolEval('text', `const rows=[...pool.querySelectorAll('.routing-model')]; rows[0].scrollIntoView({block:'center'}); const a=rows[0].querySelector('.routing-drag-handle').getBoundingClientRect(),b=rows[1].getBoundingClientRect();return {start:{x:a.left+a.width/2,y:a.top+a.height/2},end:{x:a.left+a.width/2,y:b.top+b.height*.85}};`);
  await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ ...touchPoints.start, id: 1, radiusX: 4, radiusY: 4, force: 1 }] });
  await sleep(40);
  await send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ ...touchPoints.end, id: 1, radiusX: 4, radiusY: 4, force: 1 }] });
  await sleep(40);
  await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  assert.equal(await poolEval('text', `return pool.querySelector('.routing-model').dataset.model;`), fixtureModels.text[1]);
  assert.deepEqual((await api('/api/ai/routing/text')).config, textRouting.config, 'Touch drag must remain unsaved');
  await poolEval('text', `const handle=pool.querySelector('.routing-drag-handle');handle.focus();handle.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));`);
  assert.equal(await poolEval('text', `return pool.querySelector('.routing-model').dataset.model;`), fixtureModels.text[0]);
  for (const kind of ['text', 'image']) {
    const name = kind === 'text' ? '文本' : '生图';
    const fixtureModel = 'form-' + kind + '-' + suffix;
    const countBefore = (await api('/api/ai/connections')).connections.filter(c => c.kind === kind).length;
    await poolEval(kind, `const form=pool.querySelector('.routing-new-connection'); form.open=true;
      const set=(label,value)=>{const n=[...form.querySelectorAll('[aria-label]')].find(n=>n.getAttribute('aria-label')===${JSON.stringify(name + '模型池：')}+label); if(!n)throw new Error('Missing form input '+label); n.value=value;n.dispatchEvent(new Event('input',{bubbles:true}));};
      set('连接名称',${JSON.stringify('UI独立' + name + '连接-' + suffix)}); set('API 地址（包含 /v1 等接口前缀）','http://127.0.0.1:1/v1'); set('API 密钥','local-form-fixture-key');
      set('模型名称（多个用逗号或换行分隔）',${JSON.stringify(fixtureModel)}); set('每分钟请求上限（0 为不限）','17');set('最大并发','3');
      ${kind === 'image' ? "form.querySelector('select').value='generations';" : "form.querySelector('input[type=checkbox]').checked=false;"}
      form.scrollIntoView({block:'start'});`);
    assert.equal(await poolEval(kind, `const form=pool.querySelector('.routing-new-connection'); return form.scrollWidth<=form.clientWidth+1 && pool.scrollWidth<=pool.clientWidth+1;`), true);
    assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
    await poolClick(kind, '立即保存连接');
    await waitFor(`document.querySelector('.model-routing[data-routing="${kind}"] .routing-autosave').textContent.includes('连接已自动保存')`);
    assert.equal(await poolEval(kind, `return pool.querySelector('.routing-new-connection input[type=password]').value;`), '');
    // Auto-save never collapses the form the user is editing.
    assert.equal(await poolEval(kind, `return pool.querySelector('.routing-new-connection').open;`), true);
    assert.equal(await poolEval(kind, `return [...pool.querySelector('.routing-add select').options].some(o=>o.textContent.includes(${JSON.stringify(fixtureModel)}));`), true);
    const connections = (await api('/api/ai/connections')).connections.filter(c => c.kind === kind);
    assert.equal(connections.length, countBefore + 1);
    const added = connections.find(c => c.models.includes(fixtureModel));
    assert.ok(added); assert.equal(added.base_url, 'http://127.0.0.1:1/v1'); assert.equal(added.rpm, 17); assert.equal(added.max_concurrency, 3);
    assert.equal(added.key_configured, true); assert.equal(Object.hasOwn(added, 'api_key'), false);
    if (kind === 'image') assert.equal(added.reference_api, 'generations'); else assert.equal(added.json_mode, false);
  }
  assert.deepEqual((await api('/api/ai/routing/text')).config, textRouting.config);
  assert.deepEqual((await api('/api/ai/routing/image')).config, imageRouting.config);
  assert.equal((await api('/api/ai/routing/text')).status.attempts, 0);
  assert.equal((await api('/api/ai/routing/image')).status.attempts, 0);
  assert.deepEqual(errors, []);
  console.log('[PASS] 文本/图像模型池添加、移除、排序、顺序/轮流/随机、参数保存、刷新持久化、连接池分离、桌面/390px布局与零JS异常');
  console.log('[PASS] 文本/图像新连接表单填写保存、独立localhost连接、密钥清空且不回显、模型下拉更新、参考图协议保存、390px展开表单无横溢，原有池顺序保持且无模型调用');
  console.log('[PASS] 鼠标拖拽、真实触摸Pointer Events拖拽、键盘上下排序；拖动不自动保存模型池');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(300); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
}
