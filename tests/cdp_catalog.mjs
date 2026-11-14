// Disposable server only. The provider fixture accepts GET /v1/models; no generation.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2), base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8894';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-catalog-'));
const pending = new Map(), errors = [], requests = [], port = 9347;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0;
const vendorName = '本机演示厂商-' + Date.now().toString(36);
const provider = createServer((request, response) => {
  requests.push({ method: request.method, url: request.url });
  if (request.method !== 'GET' || request.url !== '/v1/models') { response.writeHead(405); response.end(); return; }
  response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data: [{ id: 'local-library-demo' }, { id: 'remote-library-new' }] }));
});
async function api(url, method = 'GET', value) {
  const response = await fetch(base + url, { method, body: value && JSON.stringify(value), headers: value ? { 'Content-Type': 'application/json' } : undefined });
  const result = await response.json(); assert.ok(response.ok, `${url}: ${response.status} ${JSON.stringify(result)}`); return result;
}
function send(method, params = {}) { const id = ++sequence; return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); }); }
async function evaluate(expression) { const value = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }); assert.ok(!value.exceptionDetails, JSON.stringify(value.exceptionDetails)); return value.result.value; }
async function waitFor(expression) { for (let i = 0; i < 180; i++) { if (await evaluate(expression)) return; await sleep(100); } throw new Error('Timeout: ' + expression); }
async function click(text, scope = '.model-library') { await waitFor(`!!document.querySelector(${JSON.stringify(scope)}) && [...document.querySelector(${JSON.stringify(scope)}).querySelectorAll('button')].some(b=>b.textContent.trim()===${JSON.stringify(text)}&&!b.disabled)`); await evaluate(`(()=>{const root=document.querySelector(${JSON.stringify(scope)});[...root.querySelectorAll('button')].find(b=>b.textContent.trim()===${JSON.stringify(text)}&&!b.disabled).click();})()`); }
async function set(label, value) { await evaluate(`(()=>{const n=document.querySelector('[aria-label='+${JSON.stringify(JSON.stringify('模型库：' + label))}+']');if(!n)throw new Error('Missing input');n.value=${JSON.stringify(value)};n.dispatchEvent(new Event('input',{bubbles:true}));n.dispatchEvent(new Event('change',{bubbles:true}));})()`); }
async function shot(name) { fs.mkdirSync('tests/screenshots', { recursive: true }); const image = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(image.data, 'base64')); }
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  await api('/api/ai/settings', 'PATCH', { enabled: false, auto_analyze: false });
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1440,1000', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => { const message = JSON.parse(event.data); if (pending.has(message.id)) { const entry = pending.get(message.id); pending.delete(message.id); message.error ? entry.reject(message.error) : entry.resolve(message.result); } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable'); await send('Page.navigate', { url: base + '/' });
  await waitFor('!!document.querySelector("#rail-settings")'); await sleep(350); await evaluate('document.querySelector("#rail-settings").click()');
  await waitFor('!!document.querySelector(".drawer-settings.open [data-section=models]")'); await evaluate('document.querySelector(".drawer-settings [data-section=models]").click()');
  await waitFor('!!document.querySelector(".model-library .ml-layout")');
  await click('添加厂商');
  await set('厂商名称', vendorName); await set('API 地址（含 /v1 等前缀）', `http://127.0.0.1:${provider.address().port}/v1`); await set('API 密钥（留空保留）', 'catalog-ui-private-fixture');
  await click('保存厂商连接'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("厂商连接已保存")');
  let inventory = await api('/api/ai/catalog'); const vendor = inventory.vendors.find(v => v.label === vendorName); assert.ok(vendor); assert.equal(vendor.models.length, 0);
  assert.ok(!JSON.stringify(inventory).includes('catalog-ui-private-fixture'));
  await click('添加模型'); await set('模型 ID', 'local-library-demo'); await set('模型显示名', '小说分镜主模型'); await set('输入价格（可留空）', '0.8'); await set('输出价格（可留空）', '1.6');
  await evaluate(`document.querySelector(${JSON.stringify('.ml-capability-fields [aria-label="模型库：视觉"]')}).checked=true`);
  await click('保存模型'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("模型已保存")');
  inventory = await api('/api/ai/catalog'); let model = inventory.models.find(m => m.connection_id === vendor.id && m.model === 'local-library-demo');
  assert.equal(model.label, '小说分镜主模型'); assert.deepEqual(model.capabilities, ['text', 'vision']); assert.equal(model.input_price, .8);
  await click('检测', '.ml-table-row:not(.ml-table-head)'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("未执行生成请求")');
  await click('读取供应商模型'); await waitFor('!!document.querySelector(".ml-import-list input")');
  await evaluate('document.querySelector(".ml-import-list input").checked=true'); await click('导入所选模型'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("已导入 1")');
  await set('搜索模型名称、ID 或厂商', '小说分镜'); assert.equal(await evaluate('document.querySelectorAll(".ml-table-row:not(.ml-table-head)").length'), 1);
  await set('搜索模型名称、ID 或厂商', '无此模型'); assert.equal(await evaluate('document.querySelectorAll(".ml-table-row:not(.ml-table-head)").length'), 0);
  await set('搜索模型名称、ID 或厂商', '');
  await click('视觉', '.ml-filters'); assert.equal(await evaluate('document.querySelectorAll(".ml-table-row:not(.ml-table-head)").length'), 1);
  await click('全部', '.ml-filters');
  await click('编辑', `.ml-table-row[data-model-id="${model.id}"]`); await set('模型显示名', '可编辑的分镜模型'); await click('保存模型'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("模型已保存")');
  const oldPool = (await api('/api/ai/routing/text')).config;
  await api('/api/ai/routing/text', 'PATCH', { ...oldPool, enabled: false, models: [{ connection_id: vendor.id, model: model.model }] });
  await click('刷新模型库'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("模型库已刷新")');
  await evaluate(`document.querySelector('.ml-vendor[data-vendor="${vendor.id}"]').click()`);
  await waitFor('document.querySelector(".model-library").textContent.includes("文本模型池（已关闭）")');
  await click('使用中', '.ml-filters'); assert.equal(await evaluate('document.querySelectorAll(".ml-table-row:not(.ml-table-head)").length'), 1); await click('全部', '.ml-filters');
  await click('删除', `.ml-table-row[data-model-id="${model.id}"]`); await click('确认删除'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("请先从模型池")');
  await click('取消', '.ml-editor');
  await evaluate('document.querySelector(".model-library").scrollIntoView({block:"start"})'); await sleep(100);
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true); await shot('catalog-desktop');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await evaluate('document.querySelector(".model-library").scrollIntoView({block:"start"})'); await sleep(100);
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  assert.equal(await evaluate('document.querySelector(".model-library").scrollWidth<=document.querySelector(".model-library").clientWidth+1'), true); await shot('catalog-mobile');
  await click('编辑厂商'); await set('厂商名称', '已编辑厂商'); await click('保存厂商连接'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("厂商连接已保存")');
  assert.equal((await api('/api/ai/catalog')).vendors.find(v => v.id === vendor.id).key_configured, true);
  await api('/api/ai/routing/text', 'PATCH', { ...oldPool, revision: (await api('/api/ai/routing/text')).config.revision });
  await click('删除厂商'); await click('确认删除'); await waitFor('document.querySelector(".ml-feedback").textContent.includes("厂商连接已删除")');
  assert.ok(!(await api('/api/ai/catalog')).vendors.some(v => v.id === vendor.id));
  assert.deepEqual(requests, [{ method: 'GET', url: '/v1/models' }, { method: 'GET', url: '/v1/models' }]);
  assert.deepEqual(errors, []);
  console.log('[PASS] 无书模型库入口、厂商和模型CRUD、能力/价格/名称搜索、供应商列表导入、只读检测、密钥不回显、池引用删除保护、工作分配、桌面/390px布局、零JS异常');
} finally {
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  await sleep(300); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
}
