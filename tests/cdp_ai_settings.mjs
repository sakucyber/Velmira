// Run against a disposable, initially empty DATA_DIR. Search uses localhost only.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8896';
const port = Number(process.env.CDP_PORT || 9359);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-ai-settings-'));
const pending = new Map(), errors = [], searchRequests = [];
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, ws, sequence = 0;
const searchServer = createServer((request, response) => {
  const url = new URL(request.url, 'http://localhost'); searchRequests.push(url.searchParams.get('q'));
  response.writeHead(200, { 'Content-Type': 'application/json' });
  response.end(JSON.stringify({ results: [
    { title: '<b>温室资料</b>', url: 'https://example.com/greenhouse', content: '<script>window.searchInjected=true</script> 温室与园艺。' },
    { title: 'Invalid source', url: 'javascript:alert(1)', content: 'Not a link' },
  ] }));
});
async function api(url, method = 'GET', body) {
  const result = await fetch(base + url, { method, body: body && JSON.stringify(body), headers: body ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(result.ok, `${method} ${url}: ${result.status} ${await result.clone().text()}`); return result.json();
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
  for (let index = 0; index < 200; index++) { if (await evaluate(expression)) return; await sleep(80); }
  throw new Error('Timeout: ' + expression);
}
async function click(selector) {
  await waitFor(`!!document.querySelector(${JSON.stringify(selector)}) && !document.querySelector(${JSON.stringify(selector)}).disabled`);
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
}
async function input(label, value) {
  await evaluate(`(() => { const n=document.querySelector('[aria-label='+${JSON.stringify(JSON.stringify(label))}+']'); if(!n)throw new Error('Missing input'); n.value=${JSON.stringify(value)}; n.dispatchEvent(new Event('input',{bubbles:true})); n.dispatchEvent(new Event('change',{bubbles:true})); })()`);
}
async function check(label, value) {
  await evaluate(`(() => { const n=document.querySelector('[aria-label='+${JSON.stringify(JSON.stringify(label))}+']'); n.checked=${JSON.stringify(value)}; n.dispatchEvent(new Event('change',{bubbles:true})); })()`);
}
async function button(page, text) {
  await evaluate(`(() => { const b=[...document.querySelectorAll(${JSON.stringify(page + ' button')})].find(n=>n.textContent===${JSON.stringify(text)}); if(!b||b.disabled)throw new Error('Missing button '+${JSON.stringify(text)}); b.click(); })()`);
}
async function openSection(id, marker) {
  await click(`.settings-tabs [data-section="${id}"]`);
  await waitFor(`!!document.querySelector(${JSON.stringify('.settings-' + id + (marker ? ' ' + marker : ''))}) && !document.querySelector(${JSON.stringify('.settings-' + id)}).hidden`);
}
async function nav(url) { await send('Page.navigate', { url }); await waitFor('!!window.__lib'); await sleep(250); }
async function screenshot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const image = await send('Page.captureScreenshot', { format: 'png' }); fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(image.data, 'base64'));
}
function pass(message) { console.log('[PASS] ' + message); }

try {
  assert.equal((await api('/api/books')).books.length, 0, 'Use an initially empty disposable DATA_DIR');
  await new Promise(resolve => searchServer.listen(0, '127.0.0.1', resolve));
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', [
    '--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port,
    '--user-data-dir=' + profile, '--window-size=1280,900', 'about:blank',
  ], { stdio: 'ignore', windowsHide: true });
  let target;
  for (let index = 0; index < 100; index++) {
    await sleep(100);
    try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(item => item.type === 'page'); if (target) break; } catch {}
  }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(resolve => { ws.onopen = resolve; });
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (pending.has(message.id)) { const request = pending.get(message.id); pending.delete(message.id); message.error ? request.reject(message.error) : request.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails.text);
  };
  await send('Page.enable'); await send('Runtime.enable'); await nav(base + '/?debug=1');
  await click('#rail-settings'); await waitFor('!!document.querySelector(".drawer-settings.open")');
  assert.deepEqual(await evaluate('[...document.querySelectorAll(".settings-tabs button")].map(button=>button.dataset.section)'), ['reading', 'offline', 'models', 'memory', 'prompts', 'manga', 'translation', 'search', 'security']);
  assert.equal(await evaluate('document.querySelectorAll(".settings-tab-group").length'), 4);
  assert.equal(await evaluate('document.querySelectorAll(".settings-tab-group[open]").length'), 1);
  await evaluate('document.querySelector(".settings-tab-group:nth-child(2) summary").click()'); await sleep(30);
  assert.equal(await evaluate('document.querySelectorAll(".settings-tab-group[open]").length'), 1);
  assert.equal(await evaluate('document.querySelector(".settings-tab-group:nth-child(2)").open'), true);
  await openSection('offline', '[aria-label="联网后自动同步"]');
  await waitFor('document.querySelector(".settings-offline [role=status]").textContent.includes("待同步 0 条")');
  assert.ok(await evaluate('document.querySelector(".offline-book-list").textContent.includes("连接服务并导入书籍")'));
  assert.equal(await evaluate('document.querySelector("[aria-label=联网后自动同步]").checked'), true);
  await check('联网后自动同步', false);
  await openSection('models'); await waitFor('!!document.querySelector(".settings-models input, .settings-models select, .settings-models form")');
  await openSection('manga'); assert.ok(await evaluate('document.querySelector(".settings-manga").textContent.includes("请先导入")'));
  await openSection('search', '[aria-label="搜索服务"]');
  assert.equal(await evaluate('document.querySelector("[aria-label=启用联网搜索]").checked'), false);
  assert.equal(await evaluate('document.querySelector("[aria-label=自动为漫画场景检索资料]").checked'), false);
  await openSection('memory', '[aria-label="记忆标题"]');
  await openSection('prompts', '[aria-label="文本创作补充"]');
  await openSection('security'); await waitFor('!!document.querySelector(".settings-security input, .settings-security button")');
  await openSection('offline', '[aria-label="联网后自动同步"]');
  assert.equal(await evaluate('document.querySelector("[aria-label=联网后自动同步]").checked'), false);
  await check('联网后自动同步', true);
  await button('.settings-offline', '立即同步');
  await waitFor('document.querySelector(".settings-offline [role=status]").textContent.includes("本次已同步 0 条")');
  await screenshot('ai-settings-offline-desktop');
  pass('空书库可打开全部八项设置；离线页展示设备、空书库和同步状态，自动同步开关持久保存，手动同步可用');

  await openSection('memory', '[aria-label="记忆标题"]');
  await input('记忆标题', '冷幽默'); await input('记忆内容', '遇到紧张场面会说一个冷笑话。<b>不渲染为HTML</b>');
  await input('记忆标签（逗号分隔）', '性格，冷幽默'); await input('记忆类型', 'personality');
  await button('.settings-memory', '保存新记忆');
  await waitFor('document.querySelectorAll(".ai-memory-card").length===1');
  let entry = (await api('/api/ai/memories')).memories[0];
  assert.equal(entry.book_id, null); assert.deepEqual(entry.tags, ['性格', '冷幽默']);
  assert.equal(await evaluate('document.querySelectorAll(".ai-memory-content b").length'), 0);
  await click('[aria-label="停用记忆 冷幽默"]'); await waitFor(`!!document.querySelector('[aria-label="启用记忆 冷幽默"]')`);
  assert.equal((await api('/api/ai/memories')).memories[0].enabled, false);
  await click('[aria-label="启用记忆 冷幽默"]'); await waitFor(`!!document.querySelector('[aria-label="停用记忆 冷幽默"]')`);
  await click('[aria-label="编辑记忆 冷幽默"]'); await input('记忆内容', '喜欢冷幽默，遇到紧张场面会先轻声开个玩笑。');
  await button('.settings-memory', '保存记忆修改'); await waitFor('document.querySelector(".ai-memory-content").textContent.includes("轻声")');
  const edited = (await api('/api/ai/memories')).memories[0]; assert.ok(edited.revision > entry.revision);
  await input('搜索长期记忆', '不匹配'); await waitFor('document.querySelectorAll(".ai-memory-card").length===0');
  await input('搜索长期记忆', '冷幽默'); await waitFor('document.querySelectorAll(".ai-memory-card").length===1');
  await click('[aria-label="删除记忆 冷幽默"]'); await click('[aria-label="确认删除记忆 冷幽默"]'); await waitFor('document.querySelectorAll(".ai-memory-card").length===0');
  assert.equal((await api('/api/ai/memories')).memories.length, 0);
  pass('无书也可全局记忆新增、编辑、启停、搜索、确认删除；HTML以明文显示');

  await openSection('search', '[aria-label="搜索服务"]');
  await input('搜索服务', 'brave');
  assert.equal(await evaluate(`document.querySelector('[aria-label="搜索 API 地址"]').value`), 'https://api.search.brave.com');
  await input('搜索服务', 'searxng'); await input('搜索 API 地址', 'http://127.0.0.1:' + searchServer.address().port);
  await input('搜索 API 密钥（保存后显示）', 'disposable-search-secret'); await check('启用联网搜索', true);
  await button('.settings-search', '保存联网搜索设置'); await waitFor('document.querySelector(".settings-search [role=status]").textContent.includes("已保存")');
  const searchConfig = (await api('/api/ai/features')).settings;
  assert.equal(searchConfig.search.provider, 'searxng'); assert.equal(searchConfig.search.auto_enrich, false);
  assert.equal(searchConfig.search.api_key, 'disposable-search-secret');
  assert.equal(await evaluate('document.querySelector(`[aria-label="搜索 API 密钥（保存后显示）"]`).value'), 'disposable-search-secret');
  await input('搜索关键词', '温室与园艺'); await button('.settings-search', '测试联网搜索');
  await waitFor('document.querySelectorAll(".ai-search-source").length===1');
  assert.deepEqual(searchRequests, ['温室与园艺']);
  assert.equal(await evaluate('document.querySelector(".ai-search-source a").getAttribute("href")'), 'https://example.com/greenhouse');
  assert.equal(await evaluate('document.querySelector(".ai-search-source a").textContent'), '<b>温室资料</b>');
  assert.equal(await evaluate('document.querySelectorAll(".ai-search-source script, .ai-search-source b").length'), 0);
  assert.equal(await evaluate('!!window.searchInjected'), false);
  pass('本地搜索服务真实请求、可点击来源、密钥保存后明文显示、HTML安全显示及自动检索默认关闭');

  await openSection('prompts', '[aria-label="文本创作补充"]');
  await button('.settings-prompts', '重新加载设置'); await waitFor('!!document.querySelector(".settings-prompts [aria-label=文本创作补充]")');
  await input('文本创作补充', '保留原文幽默。'); await input('生图正向补充', '柔和光线，清晰线稿。'); await input('生图负向补充', '水印，多余文字。');
  await button('.settings-prompts', '保存提示词'); await waitFor('document.querySelector(".settings-prompts [role=status]").textContent.includes("已保存")');
  assert.equal((await api('/api/ai/features')).settings.prompts.image, '柔和光线，清晰线稿。');
  await screenshot('ai-settings-prompts-desktop');
  await button('.settings-prompts', '清空自定义提示词'); await waitFor('document.querySelector("[aria-label=文本创作补充]").value===""');
  assert.deepEqual((await api('/api/ai/features')).settings.prompts, { text: '', image: '', negative: '' });
  pass('文本、生图正向和负向提示词可保存及恢复默认');

  // Build genuine book/character metadata through the existing offline extraction API.
  const upload = await fetch(base + '/api/books?filename=' + encodeURIComponent('长期记忆示例.txt'), { method: 'POST', body: '第一章 温室\n\n林青走进房间，黑色短发的林青说道：你好。\n林青回头看见朋友，点头走来。', headers: { 'Content-Type': 'application/octet-stream' } });
  assert.ok(upload.ok); const book = (await upload.json()).book;
  await openSection('offline', '[aria-label="联网后自动同步"]');
  await button('.settings-offline', '刷新列表');
  await waitFor('document.querySelectorAll(".offline-book").length===1');
  await click('[aria-label="下载 ' + book.title + '"]');
  await waitFor('document.querySelector(".offline-book").textContent.includes("已下载") && document.querySelector(".offline-book button").textContent==="重新下载"');
  const downloaded = await evaluate(`(async()=>{const {localStore}=await import('/util/local-store.js');return localStore.meta(${JSON.stringify('download:' + book.id)});})()`);
  assert.equal(downloaded.complete, true); assert.ok(downloaded.count > 0);
  pass('离线设置从真实书库完整下载TXT，完成状态持久保存并显示重新下载');
  await api('/api/books/' + book.id + '/segment', 'POST', { per_chapter: 1, params: { min_chars: 1 } });
  await api('/api/books/' + book.id + '/ai/extract', 'POST', { provider: 'mock' });
  const character = (await api('/api/books/' + book.id + '/ai/characters')).characters.find(item => item.name === '林青'); assert.ok(character);
  await openSection('memory', '[aria-label="记忆标题"]'); await button('.settings-memory', '重新加载设置');
  await waitFor(`!!document.querySelector('[aria-label="记忆作用范围"] option[value="character:${character.id}"]')`);
  await input('记忆标题', '林青的性格'); await input('记忆内容', '讲话轻声，遇到温室话题会认真解释。');
  await input('记忆作用范围', 'character:' + character.id); await button('.settings-memory', '保存新记忆');
  await waitFor('document.querySelectorAll(".ai-memory-card").length===1');
  entry = (await api('/api/ai/memories')).memories[0]; assert.equal(entry.character_id, character.id); assert.equal(entry.book_id, book.id);
  await input('筛选记忆范围', 'global'); await waitFor('document.querySelectorAll(".ai-memory-card").length===0');
  await input('筛选记忆范围', 'character:' + character.id); await waitFor('document.querySelectorAll(".ai-memory-card").length===1');
  await click('[aria-label="编辑记忆 林青的性格"]'); await input('记忆作用范围', 'book:' + book.id); await button('.settings-memory', '保存记忆修改');
  await waitFor('document.querySelectorAll(".ai-memory-card").length===0');
  await input('筛选记忆范围', 'book:' + book.id); await waitFor('document.querySelectorAll(".ai-memory-card").length===1');
  entry = (await api('/api/ai/memories')).memories[0]; assert.equal(entry.character_id, null); assert.equal(entry.book_id, book.id);
  await check('启用长期记忆', false); await button('.settings-memory', '保存长期记忆设置');
  await waitFor('document.querySelector(".settings-memory [role=status]").textContent.includes("已保存")');
  assert.equal((await api('/api/ai/features')).settings.memory.enabled, false);
  pass('实际小说和Mock人物可绑定记忆，作用域切换/筛选、停用记忆注入设置生效');

  await openSection('translation');
  await waitFor('!!document.querySelector(".settings-translation [aria-label=翻译服务]")');
  const translationProviders = await evaluate('[...document.querySelectorAll(".settings-translation [aria-label=翻译服务] option")].map(option=>option.value)');
  assert.deepEqual(translationProviders, ['ai', 'bilibili', 'microsoft', 'custom']);
  await evaluate('(()=>{const n=document.querySelector(".settings-translation [aria-label=翻译服务]");n.value="bilibili";n.dispatchEvent(new Event("change",{bubbles:true}));})()');
  assert.equal(await evaluate('document.querySelector(".settings-translation [aria-label=\\"API 地址\\"]").value'), 'https://index-translate.bilibili.com/v1');
  assert.equal(await evaluate('document.querySelectorAll(".settings-translation [aria-label=\\"Microsoft 区域（可选）\\"]:not([hidden])").length'), 0);
  await evaluate('(()=>{const n=document.querySelector(".settings-translation [aria-label=翻译服务]");n.value="ai";n.dispatchEvent(new Event("change",{bubbles:true}));})()');
  await sleep(80);
  assert.equal(await evaluate('document.querySelector(".settings-translation [aria-label=\\"API 地址\\"]").closest(".ai-settings-field").hidden'), true);
  assert.equal(await evaluate('document.querySelector(".settings-translation [aria-label=\\"模型名称\\"]").closest(".ai-settings-field").hidden'), true);
  assert.equal(await evaluate('document.querySelector(".settings-translation [aria-label=\\"API 密钥\\"]").closest(".ai-settings-field").hidden'), true);
  pass('翻译服务设置支持文本模型池、Bilibili、Microsoft 和自有 API，按服务隐藏无关字段');

  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await waitFor('document.documentElement.scrollWidth<=innerWidth'); await screenshot('ai-settings-memory-mobile');
  for (const section of ['search', 'prompts', 'models', 'manga', 'translation', 'security', 'offline', 'reading']) {
    if (section === 'reading') await click('.settings-tabs [data-section=reading]'); else await openSection(section);
    assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true, 'No overflow on ' + section);
    if (section === 'offline') {
      await waitFor('document.querySelector(".offline-book").textContent.includes("已下载")');
      assert.equal(await evaluate('document.querySelector("[aria-label=联网后自动同步]").checked'), true);
      await screenshot('ai-settings-offline-mobile');
      // Exercise populated layouts too: backend permits device IDs up to 100
      // characters, while offline notes may contain long uninterrupted text.
      await api('/api/sync/device', 'POST', { device_id: 'mobile_layout_' + 'a'.repeat(86), label: 'UI布局验证设备' });
      await evaluate(`(async()=>{const {localStore}=await import('/util/local-store.js');await localStore.meta('auto-sync',false);const operation=await localStore.enqueue('annotation','upsert','local_ann_'+crypto.randomUUID(),{book_id:${JSON.stringify(book.id)},chapter:0,para:0,start_offset:0,end_offset:1,text:'林',note:'长笔记'+('uninterrupted-note-'.repeat(60)),color:'yellow'},0);await localStore.meta('sync:conflicts',[{op_id:operation.opId,entity:'annotation',entity_id:operation.entityId,reason:'revision',revision:2}]);return true;})()`);
      await click('.drawer-settings .drawer-close'); await waitFor('!document.querySelector(".drawer-settings")');
      await click('#rail-settings'); await waitFor('!!document.querySelector(".drawer-settings.open")');
      await openSection('offline', '[aria-label="联网后自动同步"]');
      await waitFor('document.querySelector(".offline-devices").textContent.includes("UI布局验证设备") && document.querySelectorAll(".offline-conflicts .ai-settings-card").length===1');
      for (const text of ['清除本机下载', '保留本机版本', '采用服务器版本', '撤销设备', '重新注册本设备']) {
        assert.ok(await evaluate(`[...document.querySelectorAll('.settings-offline button')].some(button=>button.textContent===${JSON.stringify(text)})`), 'Offline action is present: ' + text);
      }
      await click('[aria-label="清除下载 ' + book.title + '"]'); await waitFor('!!document.querySelector(".offline-remove-confirm")');
      const overflow = await evaluate('[...document.querySelectorAll(".settings-offline,.settings-offline .ai-settings-card")].filter(node=>node.scrollWidth>node.clientWidth+1).map(node=>({className:node.className,width:node.clientWidth,scroll:node.scrollWidth,text:node.textContent.slice(0,180)}))');
      if (overflow.length) await screenshot('ai-settings-offline-overflow-mobile');
      assert.deepEqual(overflow, [], 'Populated offline page/cards have no horizontal overflow');
      assert.equal(await evaluate('[...document.querySelectorAll(".settings-offline .ai-settings-card")].every(card=>card.scrollWidth<=card.clientWidth+1)'), true, 'Offline download/conflict/device cards have no horizontal overflow');
      await evaluate('document.querySelector(".offline-conflicts").scrollIntoView({block:"start"})'); await screenshot('ai-settings-offline-conflicts-mobile');
      await evaluate('document.querySelector(".offline-devices").scrollIntoView({block:"start"})'); await screenshot('ai-settings-offline-devices-mobile');
      await evaluate('[...document.querySelectorAll(".offline-remove-confirm button")].find(button=>button.textContent==="取消").click()');
      pass('390px离线清缓存确认、长正文冲突卡、最长设备ID、撤销与重新注册入口无横向溢出');
    }
  }
  await screenshot('ai-settings-navigation-mobile');
  assert.deepEqual(errors, []); pass('390px全部设置无横向溢出，全程无JavaScript异常');
} finally {
  searchServer.closeAllConnections?.(); await new Promise(resolve => searchServer.close(resolve));
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(200); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}
