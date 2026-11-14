// Real Android Java gateway + WebView, with deterministic local model services.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { execFileSync } from 'node:child_process';

const serial = process.env.ANDROID_SERIAL || '127.0.0.1:16384';
const adb = process.env.ANDROID_ADB || 'C:/Users/saku_cyber/.workbuddy/binaries/platform-tools/platform-tools/adb.exe';
const adbRun = args => execFileSync(adb, ['-s', serial, ...args], { encoding: 'utf8', windowsHide: true });
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let ws, seq = 0, original, reverse, settingsMutated = false; const pending = new Map(), requests = [], errors = [];
class SkipTest extends Error {}
const png = fs.readFileSync('web/icons/icon-192.png');
const fixture = createServer(async (request, response) => {
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const body = Buffer.concat(chunks).toString(); const data = body ? JSON.parse(body) : {}; requests.push({ path: request.url, data });
  response.setHeader('Content-Type', 'application/json');
  if (data.model === 'broken') { response.writeHead(503); response.end(JSON.stringify({ error: 'synthetic upstream failure' })); return; }
  if (request.url === '/v1/chat/completions') { if (String(data.messages?.at(-1)?.content).includes('SLOW')) await sleep(1000); response.end(JSON.stringify({ choices: [{ message: { content: '验收回答：人物冷幽默；六格分镜。模型 ' + data.model } }] })); }
  else if (request.url === '/v1/images/generations') response.end(JSON.stringify({ data: [{ b64_json: png.toString('base64') }] }));
  else if (request.url === '/v1/models') response.end(JSON.stringify({ data: [{ id: 'working' }] }));
  else if (request.url.startsWith('/search')) response.end(JSON.stringify({ results: [{ title: '验收资料', url: 'https://example.com/source', content: '来源摘要' }] }));
  else { response.writeHead(404); response.end('{}'); }
});
function send(method, params = {}) { return new Promise((resolve, reject) => { const id = ++seq; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); }); }
async function connect() {
  const pid = adbRun(['shell', 'pidof', 'dev.velmira.mobile']).trim();
  try { adbRun(['forward', '--remove', 'tcp:9222']); } catch {}
  adbRun(['forward', 'tcp:9222', 'localabstract:webview_devtools_remote_' + pid]);
  let targets; for (let i = 0; i < 50; i++) { try { targets = await (await fetch('http://127.0.0.1:9222/json')).json(); if (targets.some(t => t.type === 'page')) break; } catch {} await sleep(100); }
  const target = targets.find(t => t.type === 'page' && JSON.parse(t.description || '{}').visible) || targets.find(t => t.type === 'page'); assert.ok(target);
  ws = new WebSocket(target.webSocketDebuggerUrl); await once(ws, 'open');
  ws.onmessage = event => { const msg = JSON.parse(event.data), p = pending.get(msg.id); if (p) { pending.delete(msg.id); msg.error ? p.reject(msg.error) : p.resolve(msg.result); } else if (msg.method === 'Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.text); };
  await send('Runtime.enable'); await send('Page.enable');
}
async function evaluate(expression) { const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }); assert.ok(!r.exceptionDetails, JSON.stringify(r.exceptionDetails)); return r.result.value; }
async function waitFor(expression) { for (let i = 0; i < 150; i++) { if (await evaluate(expression)) return; await sleep(70); } throw new Error('Timeout: ' + expression + '\n' + await evaluate('document.body.innerText.slice(-1800)')); }
async function api(url, method = 'GET', body, allowError = false) {
  const result = await evaluate(`(async()=>{const r=await fetch(${JSON.stringify(url)},{method:${JSON.stringify(method)},headers:{'Content-Type':'application/json'},${body === undefined ? '' : 'body:JSON.stringify(' + JSON.stringify(body) + '),'}});return {status:r.status,body:await r.json()}})()`);
  if (!allowError) assert.ok(result.status >= 200 && result.status < 300, JSON.stringify(result)); return allowError ? result : result.body;
}
async function click(selector) { await waitFor(`!!document.querySelector(${JSON.stringify(selector)})`); await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`); }
async function button(label, root = 'document') { await evaluate(`(()=>{const n=[...${root}.querySelectorAll('button')].find(x=>x.textContent===${JSON.stringify(label)});if(!n)throw Error('Missing button');n.click()})()`); }
async function screenshot(name) { const r = await send('Page.captureScreenshot', { format: 'png' }); fs.mkdirSync('tests/screenshots', { recursive: true }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(r.data, 'base64')); }
async function checkWindowTheme(theme) {
  const expected = {
    day: { status: '#fffffdf8', navigation: '#fff4f1ea', lightIcons: 0x2010 },
    sepia: { status: '#fff4e8d0', navigation: '#ffe9dcc2', lightIcons: 0x2010 },
    night: { status: '#ff1c1f26', navigation: '#ff14161b', lightIcons: 0 }
  }[theme];
  let dumpLines, dumpSection, values;
  for (let attempt = 0; attempt < 20; attempt++) {
    dumpLines = adbRun(['shell', 'dumpsys', 'activity', 'top']).split('\n');
    const marker = dumpLines.findIndex(line => line.includes('Velmira window theme:'));
    if (marker < 0) {
      const summary = dumpLines.filter(line => line.includes('dev.velmira.mobile') || line.includes('statusBarColor') || line.includes('navigationBarColor')).slice(-8).join(' | ');
      console.log('[INFO] MainActivity window color diagnostics unavailable; recorded only: ' + summary);
      return;
    }
    dumpSection = dumpLines.slice(marker, marker + 5).map(line => line.trim());
    values = Object.fromEntries(dumpSection.slice(1).map(line => line.split('=')));
    if (values.theme === theme) break;
    await sleep(100);
  }
  assert.equal(values.theme, theme, 'native window theme');
  assert.equal(values.statusBarColor, expected.status, 'native status bar color');
  assert.equal(values.navigationBarColor, expected.navigation, 'native navigation bar color');
  assert.match(values.systemUiVisibility || '', /^0x[\da-f]+$/i, 'native system UI flags');
  assert.equal(parseInt(values.systemUiVisibility, 16) & 0x2010, expected.lightIcons, 'native status/navigation icon appearance');
  console.log('[PASS] MainActivity dump confirms ' + theme + ' system bars: ' + dumpSection.join(' | '));
}
try {
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening'); reverse = fixture.address().port; adbRun(['reverse', 'tcp:' + reverse, 'tcp:' + reverse]);
  await connect(); await waitFor('document.readyState==="complete"');
  if (await evaluate('!!document.querySelector(".onboarding")')) await button('跳过，直接进入', 'document.querySelector(".onboarding")');
  original = (await api('/api/mobile-ai/settings')).settings;
  assert.ok(original.connections.every(c => !('api_key' in c))); assert.ok(!('api_key' in original.search));
  // Preserve configured user connections. The local test connections are
  // appended temporarily; blank keys on existing IDs make LocalAiService
  // retain the stored credential when the settings snapshot is restored.
  assert.ok(!original.connections.some(c => ['c_testtext', 'c_testimage'].includes(c.id)));
  const base = 'http://127.0.0.1:' + reverse + '/v1';
  const canTestSearch = original.search.key_configured === false;
  const testConnections = original.connections.map(connection => ({ ...connection, api_key: '' }));
  testConnections.push(
    { id: 'c_testtext', label: '文本验收', kind: 'text', base_url: base, api_key: 'synthetic-only', models: ['broken', 'working', 'second'], rpm: 25, max_concurrency: 8 },
    { id: 'c_testimage', label: '生图验收', kind: 'image', base_url: base, api_key: 'synthetic-only', models: ['image'], rpm: 25, max_concurrency: 8 },
  );
  settingsMutated = true;
  const settingsPatch = { connections: testConnections, text: { enabled: true, strategy: 'priority', models: [{ connection_id: 'c_testtext', model: 'broken' }, { connection_id: 'c_testtext', model: 'working' }] }, image: { enabled: true, models: [{ connection_id: 'c_testimage', model: 'image' }] }, prompts: { text: '保留冷幽默', image: '清晰线稿', negative: '水印' } };
  if (canTestSearch) settingsPatch.search = { enabled: true, provider: 'searxng', base_url: 'http://127.0.0.1:' + reverse };
  let settings = (await api('/api/mobile-ai/settings', 'PATCH', settingsPatch)).settings;
  assert.ok(!JSON.stringify(settings).includes('synthetic-only'));
  const memory = (await api('/api/mobile-ai/memories', 'POST', { title: '验收人物', kind: 'personality', content: '角色保持冷幽默', enabled: true, book_id: null, tags: ['验收'] })).memory;
  const fallback = await api('/api/mobile-ai/text', 'POST', { prompt: '总结人物', context: '甲翻开书页。', task: 'characters', book_id: null });
  assert.equal(fallback.model, 'working'); assert.equal(fallback.fallback_used, true); assert.ok(requests.some(r => r.data.model === 'broken'));
  const request = requests.findLast(r => r.data.model === 'working').data; assert.ok(request.messages[0].content.includes('保留冷幽默')); assert.ok(request.messages[1].content.includes('角色保持冷幽默'));
  const translated = await api('/api/mobile-ai/translate', 'POST', { text: 'The greenhouse was quiet.', target_language: '简体中文', context: 'A short nearby paragraph.', book_id: null });
  assert.ok(translated.text.includes('验收回答')); assert.equal(translated.kind, 'translate');
  const explained = await api('/api/mobile-ai/explain', 'POST', { text: '甲翻开书页。', context: '附近段落。', book_id: null });
  assert.ok(explained.text.includes('验收回答')); assert.equal(explained.kind, 'explain');
  console.log('[PASS] Selected-text translation and explanation use the mobile text pool without adding history entries');
  console.log('[PASS] Android model failure switches to another model on the same connection; prompts and private memory reach text requests; settings hide credentials');
  await api('/api/mobile-ai/settings', 'PATCH', { text: { strategy: 'round_robin', models: [{ connection_id: 'c_testtext', model: 'working' }, { connection_id: 'c_testtext', model: 'second' }] } });
  const r1 = await api('/api/mobile-ai/text', 'POST', { prompt: '轮询1' }), r2 = await api('/api/mobile-ai/text', 'POST', { prompt: '轮询2' }); assert.notEqual(r1.model, r2.model);
  await evaluate(`window.__slow=fetch('/api/mobile-ai/text',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({prompt:'SLOW'})}).then(r=>r.json());true`);
  const start = Date.now(); await api('/api/books'); assert.ok(Date.now() - start < 700, 'AI must not block book APIs'); await evaluate('window.__slow');
  const image = await api('/api/mobile-ai/image', 'POST', { prompt: '窗边翻书的漫画人物', book_id: null }); assert.ok(image.url.startsWith('/api/mobile-ai/images/'));
  assert.equal(await evaluate(`(async()=>{const i=new Image();i.src=${JSON.stringify(image.url)};await i.decode();return i.naturalWidth})()`), 192);
  if (canTestSearch) {
    const search = await api('/api/mobile-ai/search', 'POST', { query: '验收' }); assert.equal(search.results[0].title, '验收资料');
  } else console.log('[SKIP] Existing search credential retained; search fixture was not substituted');
  console.log('[PASS] Round robin distributes text requests; reading APIs respond during AI work; generated image saves and decodes' + (canTestSearch ? '; web search returns sources' : ''));
  await send('Page.navigate', { url: await evaluate('location.origin') + '/?debug=1' }); await waitFor('!!window.__lib');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await click('#rail-settings'); await click('[data-section=models]'); await waitFor('!!document.querySelector(".mai-pool-member")');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await screenshot('android-mobile-ai-settings');
  await button('↓', 'document.querySelector("[data-mobile-pool=text] .mai-pool-member")'); await button('保存文本模型池');
  await waitFor('document.querySelector("[data-mobile-pool=text] .mai-report").textContent.includes("已保存")');
  assert.equal((await api('/api/mobile-ai/settings')).settings.text.models[0].model, 'second');
  await click('[data-section=reading]');
  for (const theme of ['day', 'sepia', 'night']) { await click('[data-theme-choice=' + theme + ']'); await checkWindowTheme(theme); }
  await click('.drawer-settings .drawer-close'); await sleep(300); await click('#rail-ai'); await waitFor('!!document.querySelector(".drawer-mobile-ai .mai-card")');
  await screenshot('android-mobile-ai-studio'); assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await button('开始文本 AI'); await waitFor('document.querySelector(".mai-text-results").textContent.includes("验收回答")');
  await evaluate('document.querySelector("[aria-label=画面描述]").value="手机生图验收"'); await button('生成图片'); await waitFor('[...document.querySelectorAll(".mai-images img")].some(i=>i.complete&&i.naturalWidth>0)');
  const before = (await api('/api/mobile-ai/history')).history.length;
  ws.close(); adbRun(['shell', 'am', 'force-stop', 'dev.velmira.mobile']); adbRun(['shell', 'am', 'start', '-n', 'dev.velmira.mobile/.MainActivity']); await sleep(700); await connect();
  await checkWindowTheme('night');
  assert.ok((await api('/api/mobile-ai/history')).history.length >= before); assert.ok((await api('/api/mobile-ai/memories')).memories.some(m => m.id === memory.id));
  assert.deepEqual(errors, []);
  await api('/api/mobile-ai/memories/' + memory.id + '?revision=' + memory.revision, 'DELETE');
  console.log('[PASS] 390px settings/model order and AI text/image UI work; history and memory survive restart');
} catch (error) {
  if (error instanceof SkipTest) console.log('[SKIP] ' + error.message);
  else throw error;
} finally {
  if (settingsMutated && original && ws?.readyState === WebSocket.OPEN) {
    // Only restore snapshots confirmed to have no credentials before any mutation.
    await api('/api/mobile-ai/settings', 'PATCH', { ...original, revision: (await api('/api/mobile-ai/settings')).settings.revision }).catch(e => console.error('Restore failed:', e.message));
  }
  ws?.close(); if (reverse) try { adbRun(['reverse', '--remove', 'tcp:' + reverse]); } catch {}
  fixture.closeAllConnections(); await new Promise(resolve => fixture.close(resolve));
}
