// Disposable backend only. Uses a local fake text API and the existing mock image provider.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';

const args = process.argv.slice(2);
const base = args.includes('--base') ? args[args.indexOf('--base') + 1] : 'http://127.0.0.1:8886';
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-auto-'));
const port = 9343, pending = new Map(), errors = [], materials = [];
const sleep = ms => new Promise(r => setTimeout(r, ms));
let browser, ws, sequence = 0, savedSettings, failSummary = 2;
const textAPI = createServer(async (request, response) => {
  try {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    const system = input.messages[0].content, material = JSON.parse(input.messages[1].content);
    if (system.includes('一次编排完整一页') && failSummary > 0) {
      failSummary--; response.writeHead(503); response.end('Temporary fixture failure'); return;
    }
    let output;
    if (system.includes('整书阅读分析员')) output = {
      synopsis: '林澄沿小路走入花园，随后在长椅旁停下。', summary: '林澄来到花园', kind: 'scene', location: 'garden', time_of_day: 'day', entities: ['花园'],
      characters: [{ name: '林澄', card: { hair_color: 'black' }, evidence: [{ field: 'hair_color', value: 'black', quote: '黑色短发' }] }],
    };
    else if (system.includes('章节连续性记忆编辑器')) {
      const byteBudget = String(material.token_count_method || '').startsWith('estimated:utf8-bytes');
      output = { memory: byteBudget
        ? '林澄留着黑色短发，沿小路走入花园，随后在长椅旁停下。本章只明确人物外貌、花园与长椅位置，没有其他身份变化、关系变化或待解悬念。'
        : 'Lin Cheng has short black hair and walks along the path into the garden. He then stops beside a bench. The sequence establishes his current appearance and location. There is no stated change of identity, clothing, relationship, possession or unresolved promise. Later illustrations must preserve the short black hair and depict the walk before the stop. The garden path leads to the bench, and the bench is the stated destination of this action. No weather, additional people, hidden motives or future events are established in the source. This is a neutral continuity record for the next chapter.', facts: [] };
    }
    else if (system.includes('漫画分页导演')) output = { pages: [{ start_sentence: 0, end_sentence: material.sentences.length - 1, panels: 2, layout: 'auto' }] };
    else if (system.includes('连载漫画编剧')) {
      materials.push(material);
      output = { panels: [
        { shot: 'medium', action: 'walk into the garden in daylight', caption: '林澄沿小路走进花园。', start_sentence: 0, end_sentence: 0 },
        { shot: 'reaction', action: 'stop beside a bench after walking', caption: '随后在长椅旁停下。', start_sentence: 1, end_sentence: material.sentences.length - 1 },
      ].map(p => ({ ...p, kind: 'solo', subjects: [{ character_id: material.characters[0].id, position: 'center', pose: p.action }], environment: 'garden with a bench', lighting: 'daylight', emotion: '', source_quote: material.sentences[p.start_sentence].text, dialogue: '', speaker: '' })) };
    } else if (system.includes('上下文总结编辑')) output = { summary: '林澄走进花园后在长椅旁停下，动作承接上一格。', location: 'garden', moment: 'stop', visible_action: 'stop beside a bench' };
    else if (system.includes('single comic-panel')) output = { visual_prompt: 'A single adult with black short hair stands beside a bench in a quiet garden in daylight, continuing the previous walk, face clearly visible.' };
    else throw new Error('Unexpected text stage');
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify(output) } }] }));
  } catch (err) { response.writeHead(500); response.end('Fixture output failed'); }
});
async function api(url, method = 'GET', data) {
  const r = await fetch(base + url, { method, body: data && JSON.stringify(data), headers: data ? { 'Content-Type': 'application/json' } : undefined });
  assert.ok(r.ok, `${url}: ${r.status} ${await r.clone().text()}`); return r.json();
}
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!r.exceptionDetails, JSON.stringify(r.exceptionDetails)); return r.result.value;
}
async function waitFor(expression) {
  for (let i = 0; i < 300; i++) { if (await evaluate(expression)) return; await sleep(100); }
  throw new Error('Timeout: ' + expression);
}
async function click(selector) {
  await waitFor(`!!document.querySelector(${JSON.stringify(selector)}) && !document.querySelector(${JSON.stringify(selector)}).disabled`);
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
}
async function shot(name) {
  fs.mkdirSync('tests/screenshots', { recursive: true });
  const r = await send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync('tests/screenshots/' + name + '.png', Buffer.from(r.data, 'base64'));
}
try {
  await new Promise(resolve => textAPI.listen(0, '127.0.0.1', resolve));
  savedSettings = (await api('/api/ai/settings')).settings;
  await api('/api/ai/settings', 'PATCH', { enabled: true, auto_analyze: false, base_url: `http://127.0.0.1:${textAPI.address().port}/v1`, model: 'gpt-oss-120b' });
  const text = '第一章 花园\n\n林澄留着黑色短发，沿小路走进花园。随后，林澄在长椅旁停下。\n\n第二章 小路\n\n' + Array.from({length: 10}, (_, i) => '林澄留着黑色短发，又沿小路走进花园。随后，林澄在长椅旁停下。第' + (i + 1) + '段。').join('\n\n') + '\n\n验证批次' + Date.now() + '。';
  const upload = await fetch(base + '/api/books?filename=自动漫画示例.txt', { method: 'POST', body: text, headers: { 'Content-Type': 'application/octet-stream' } });
  assert.ok(upload.ok); const book = (await upload.json()).book;
  const prefix = '/api/books/' + book.id + '/manga';
  await api(prefix + '/config', 'PATCH', { provider: 'mock', width: 256, height: 256, review_storyboards: true });
  browser = spawn(process.env.EDGE_PATH || 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', ['--headless=new', '--disable-gpu', '--no-first-run', '--remote-debugging-port=' + port, '--user-data-dir=' + profile, '--window-size=1280,860', 'about:blank'], { stdio: 'ignore' });
  let target;
  for (let i = 0; i < 100; i++) { await sleep(100); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(t => t.type === 'page'); if (target) break; } catch {} }
  assert.ok(target); ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise(r => { ws.onopen = r; });
  ws.onmessage = e => { const m = JSON.parse(e.data); if (pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); } else if (m.method === 'Runtime.exceptionThrown') errors.push(m.params.exceptionDetails.text); };
  await send('Page.enable'); await send('Runtime.enable');
  await send('Page.navigate', { url: base + '/?book=' + book.id + '&ch=1&debug=1' });
  await waitFor('!!window.__velmira && !!document.querySelector(".auto-manga-toggle") && !document.querySelector(".auto-manga-toggle").disabled');
  await click('#btn-ai'); await waitFor('!!document.querySelector(".drawer-ai.open")');
  assert.equal(await evaluate('document.querySelectorAll(".ai-row").length'), 2);
  assert.ok(await evaluate('document.querySelectorAll(".ai-row")[1].getBoundingClientRect().top >= document.querySelectorAll(".ai-row")[0].getBoundingClientRect().bottom'));
  await click('[data-ai-section=characters]');
  assert.equal(await evaluate('document.querySelector(".ai-analysis-section").hidden && !document.querySelector(".ai-characters-section").hidden'), true);
  await click('.drawer-ai .drawer-close'); await waitFor('!document.querySelector(".drawer-ai.open")');
  await shot('automatic-reader-desktop');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await evaluate('window.__velmira.jumpTo({chapter:1, para:5})');
  const readingPara = await evaluate('window.__velmira.pager.locatePara()');
  await click('.auto-manga-toggle');
  await waitFor('!!document.querySelector(".drawer-automatic.open [role=switch]")');
  assert.equal((await api(prefix + '/auto')).auto.start_chapter, 1);
  await evaluate('[...document.querySelectorAll(".drawer-automatic button")].find(b=>b.textContent==="漫画与画风设置").click()');
  await waitFor('!!document.querySelector(".drawer-settings.open [aria-label=图像后端]")');
  assert.equal(await evaluate('document.querySelector(".settings-tabs [data-section=manga]").getAttribute("aria-pressed")'), 'true');
  assert.equal(await evaluate('document.querySelector(".settings-reading").hidden'), true);
  await sleep(300); await shot('automatic-settings-jump');
  await click('.drawer-settings .drawer-close'); await waitFor('!document.querySelector(".drawer-settings.open")');
  await click('.auto-manga-view'); await waitFor('!!document.querySelector(".drawer-automatic.open [role=switch]")');
  await click('.drawer-automatic [role=switch]');
  await waitFor('document.querySelector(".drawer-automatic [role=switch]").getAttribute("aria-checked")==="false"');
  const paused = (await api(prefix + '/auto')).auto;
  assert.equal(paused.status, 'paused');
  await click('.drawer-automatic [role=switch]');
  const resumed = (await api(prefix + '/auto')).auto;
  assert.equal(resumed.run_id, paused.run_id);
  await waitFor('document.querySelectorAll(".drawer-automatic .storyboard-card").length===2');
  await waitFor('document.querySelector(".drawer-automatic [role=status]").textContent.includes("等待分镜确认")');
  await evaluate('(() => { const input=document.querySelector(".drawer-automatic [aria-label=中文旁白]"); input.value="先走入花园。"; input.dispatchEvent(new Event("input",{bubbles:true})); })()');
  await evaluate('[...document.querySelectorAll(".drawer-automatic button")].find(b=>b.textContent==="保存分镜修改").click()');
  await waitFor('document.querySelector(".storyboard-editor [role=status]").textContent.includes("版本 2")');
  assert.equal((await api(prefix + '/jobs')).jobs.length, 0);
  await shot('storyboard-editor-desktop');
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await waitFor('document.documentElement.scrollWidth<=innerWidth');
  await shot('storyboard-editor-mobile');
  await send('Emulation.clearDeviceMetricsOverride');
  await evaluate('[...document.querySelectorAll(".drawer-automatic button")].find(b=>b.textContent==="确认本页并继续").click()');
  for (let i = 0; i < 120; i++) {
    const state = (await api(prefix + '/auto')).auto;
    assert.notEqual(state.status, 'error', JSON.stringify(state));
    if (state.status === 'done') break;
    await sleep(500);
  }
  assert.equal((await api(prefix + '/auto')).auto.status, 'done');
  await waitFor('!!document.querySelector(".automatic-page img") && document.querySelector(".automatic-page img").complete && document.querySelector(".automatic-page img").naturalWidth>0');
  await waitFor('document.querySelectorAll(".automatic-log").length>0');
  const logs = (await api(prefix + '/auto/logs')).logs;
  assert.ok(logs.some(l=>l.message.includes('漫画页已保存')));
  assert.ok(logs.some(l=>l.level==='warning' && l.message.includes('失败')));
  assert.ok(logs.some(l=>/第 [23]\/3 次尝试/.test(l.message)));
  await shot('automatic-draft-desktop');
  await click('.drawer-automatic .drawer-close'); await waitFor('!document.querySelector(".drawer-automatic.open")');
  await waitFor('!!document.querySelector(".reader-comics img") && document.querySelector(".reader-comics img").naturalWidth>0');
  assert.ok(await evaluate('document.querySelector(".reader-comics").getBoundingClientRect().left >= document.querySelector("#reader-viewport").getBoundingClientRect().right - 1'));
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await sleep(300);
  assert.ok(Math.abs(await evaluate('window.__velmira.pager.locatePara()') - readingPara) <= 10);
  await shot('automatic-text-comic-desktop');
  // The left reading position must advance inside the same panel, pause at
  // that exact position and survive returning to manual reading.
  await evaluate('window.__velmira.jumpTo({chapter:1,para:0,offset:0})');
  await evaluate('[...document.querySelectorAll(".reading-sync-controls button")].find(b=>b.textContent==="自动播放").click()');
  await waitFor('document.querySelector(".reading-sync-controls button.active")?.textContent==="自动播放"');
  await sleep(800);
  const playingPosition = await evaluate('({para:window.__velmira.state.para,offset:window.__velmira.state.offset})');
  assert.equal(playingPosition.para, 0);
  assert.ok(playingPosition.offset > 0 && playingPosition.offset < 18, JSON.stringify(playingPosition));
  await evaluate('[...document.querySelectorAll(".reading-sync-controls button")].find(b=>b.textContent==="暂停").click()');
  const pausedPosition = await evaluate('({para:window.__velmira.state.para,offset:window.__velmira.state.offset})');
  await sleep(800);
  assert.deepEqual(await evaluate('({para:window.__velmira.state.para,offset:window.__velmira.state.offset})'), pausedPosition);
  await evaluate('[...document.querySelectorAll(".reading-sync-controls button")].find(b=>b.textContent==="手动").click()');
  assert.equal(await evaluate('document.querySelector(".reading-sync-controls button.active")?.textContent'), '手动');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  await shot('synchronized-reading-controls-desktop');
  await click('.auto-manga-view'); await waitFor('!!document.querySelector(".drawer-automatic.open")');
  const pages = (await api(prefix + '/auto/pages')).pages;
  assert.ok(pages.length > 0); assert.ok(pages.every(p => p.chapter === 1 && p.status === 'draft' && p.pending_review > 0));
  assert.ok(materials.length > 0);
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await waitFor('document.documentElement.scrollWidth<=innerWidth');
  await shot('automatic-draft-mobile');
  await click('.drawer-automatic .drawer-close');
  await waitFor('!document.querySelector(".drawer-automatic.open")');
  assert.equal(await evaluate('document.documentElement.scrollWidth<=innerWidth'), true);
  assert.ok(await evaluate('document.querySelector(".auto-manga-toggle").getBoundingClientRect().width>=80'));
  assert.ok(await evaluate('document.querySelector(".reader-comics").getBoundingClientRect().top >= document.querySelector("#reader-viewport").getBoundingClientRect().bottom - 1'));
  assert.equal(await evaluate('[...document.querySelectorAll(".reading-sync-controls button")].filter(b=>!b.hidden).every(b=>{const r=b.getBoundingClientRect();return r.width>=40&&r.left>=0&&r.right<=innerWidth&&r.bottom<=innerHeight;})'), true);
  assert.equal(await evaluate('document.querySelector("#page-label").getBoundingClientRect().bottom<=innerHeight'), true);
  await sleep(300); await shot('automatic-reader-mobile');
  await send('Page.navigate', { url: base + '/?book=' + book.id + '&ch=1&debug=1' });
  await waitFor('!!window.__velmira && !!document.querySelector(".auto-manga-view")');
  await waitFor('!!document.querySelector(".reader-comics img")');
  await click('.auto-manga-view');
  await waitFor('!!document.querySelector(".automatic-page img")');
  await waitFor('document.querySelectorAll(".automatic-log").length>0');
  assert.deepEqual(errors, []);
  console.log('[PASS] 阅读页开关、当前章起点、暂停续跑、真实整书分析与出图队列、草稿拼页、三次尝试日志、设置直达、AI/人物卡两行入口、正文与漫画并排、同步阅读字符推进与精确暂停、刷新持久化、390px 控件布局与零 JS 异常');
  console.log('Preview: ' + base + '/?book=' + book.id + '&ch=1');
} finally {
  if (savedSettings) {
    const { enabled, auto_analyze, base_url, model } = savedSettings;
    await api('/api/ai/settings', 'PATCH', { enabled, auto_analyze, ...(base_url ? { base_url } : {}), ...(model ? { model } : {}) });
  }
  textAPI.closeAllConnections(); await new Promise(resolve => textAPI.close(resolve));
  if (ws?.readyState === WebSocket.OPEN) { await send('Browser.close').catch(() => {}); ws.close(); }
  browser?.kill(); await sleep(300); fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
}
