// Opt-in live smoke test. Transfers existing private desktop credentials directly
// to the selected emulator over CDP; never writes or prints credential material.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { once } from 'node:events';
import { execFileSync, spawnSync } from 'node:child_process';

assert.ok(process.argv.includes('--use-desktop-credentials'), 'Live calls require --use-desktop-credentials');
const adb = process.env.ANDROID_ADB || 'C:/Users/saku_cyber/.workbuddy/binaries/platform-tools/platform-tools/adb.exe';
const serial = process.env.ANDROID_SERIAL || '127.0.0.1:16384';
const run = args => execFileSync(adb, ['-s', serial, ...args], { encoding: 'utf8', windowsHide: true });
const source = spawnSync(process.env.PYTHON || 'python', ['-c', `
import json
from server.app.ai import connections, settings
allowed = {'agnes', 'knd-text', 'pie-image', 'pie-text'}
rows=[]
for row in connections.entries():
    if row['id'] not in allowed: continue
    private=settings.load_settings('profile-'+row['id'])
    key=settings.api_key(private)
    if key: rows.append({**row,'base_url':private['base_url'],'api_key':key})
print(json.dumps(rows))
`], { encoding: 'utf8', windowsHide: true });
assert.equal(source.status, 0, 'Private desktop credential loader failed');
let credentials;
try { credentials = JSON.parse(source.stdout); } catch { throw Error('Private credential loader returned invalid data'); }
const allowedHosts = { agnes: 'apihub.agnes-ai.com', 'knd-text': 'kndnadi.org', 'pie-image': 'api.pie-xian.com', 'pie-text': 'api.pie-xian.com' };
for (const entry of credentials) {
  const url = new URL(entry.base_url);
  assert.equal(url.protocol, 'https:'); assert.equal(url.hostname, allowedHosts[entry.id]);
}
assert.ok(credentials.some(c => c.id === 'agnes') && credentials.some(c => c.id === 'knd-text'), 'Default provider credentials are not configured on the desktop');
const appProcess = run(['shell', 'pidof', 'dev.velmira.mobile']).trim();
try { run(['forward', '--remove', 'tcp:9222']); } catch {}
run(['forward', 'tcp:9222', 'localabstract:webview_devtools_remote_' + appProcess]);
const targets = await (await fetch('http://127.0.0.1:9222/json')).json();
const target = targets.find(t => t.type === 'page' && t.url.startsWith('http://127.0.0.1:18765/'));
assert.ok(target, 'Open the standalone local library first');
let socket, sequence = 0; const pending = new Map();
function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence, timer = setTimeout(() => { pending.delete(id); reject(Error('Device operation timed out')); }, 360000);
    pending.set(id, { resolve, reject, timer }); socket.send(JSON.stringify({ id, method, params }));
  });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  assert.ok(!result.exceptionDetails, 'Device JavaScript failed'); return result.result.value;
}
async function api(path, method = 'GET', data) {
  const result = await evaluate(`(async()=>{const r=await fetch(${JSON.stringify(path)},{method:${JSON.stringify(method)},headers:{'Content-Type':'application/json'},${data === undefined ? '' : 'body:JSON.stringify(' + JSON.stringify(data) + '),'}});return {status:r.status,body:await r.json()}})()`);
  assert.ok(result.status >= 200 && result.status < 300, 'Mobile API failed: ' + result.status + ' ' + (result.body.error?.code || 'unknown') + ' ' + (result.body.error?.message || ''));
  return result.body;
}
try {
  socket = new WebSocket(target.webSocketDebuggerUrl); await once(socket, 'open');
  socket.onmessage = event => { const m = JSON.parse(event.data), item = pending.get(m.id); if (item) { pending.delete(m.id); clearTimeout(item.timer); m.error ? item.reject(Error('Device protocol error')) : item.resolve(m.result); } };
  const original = (await api('/api/mobile-ai/settings')).settings;
  const mapped = { 'knd-text': 'c_luna', agnes: 'c_agnes', 'pie-text': 'c_public_text', 'pie-image': 'c_relay_image' };
  const additions = credentials.map(c => ({ ...c, id: mapped[c.id] }));
  const connections = original.connections.filter(c => !additions.some(a => a.id === c.id)).concat(additions);
  const members = (kind, ids, preferred) => ids.flatMap(id => {
    const c = connections.find(c => c.id === id && c.kind === kind);
    return c ? preferred.filter(model => c.models.includes(model)).map(model => ({ connection_id: id, model })) : [];
  });
  const text = { ...original.text, enabled: true, strategy: 'priority', models: members('text', ['c_luna', 'c_public_text'], ['gpt-6-luna', 'qwen3.8-27b', 'gpt-oss-120b']) };
  const image = { ...original.image, enabled: true, strategy: 'priority', models: members('image', ['c_agnes', 'c_relay_image'], ['agnes-image-2.5-flash', 'agnes-image-2.1-flash', 'agnes-image-2.0-flash']) };
  const saved = (await api('/api/mobile-ai/settings', 'PATCH', { revision: original.revision, connections, text, image })).settings;
  assert.equal(saved.text.models[0].model, 'gpt-6-luna'); assert.equal(saved.image.models[0].model, 'agnes-image-2.5-flash');
  assert.ok(saved.connections.every(c => !('api_key' in c)));
  credentials = null; source.stdout = ''; additions.forEach(c => { delete c.api_key; });
  console.log('[PASS] Default Luna/Agnes pools and adjustable backups configured privately on emulator; APK remains credential-free');
  const answer = await api('/api/mobile-ai/text', 'POST', { prompt: '请仅输出一句中文，概括原文中发生的事情，不补充细节。', context: '林舟走进旧书店，买下一本蓝色封面的小说。', task: 'summary' });
  assert.ok(answer.text.length > 5 && /林舟|书店|小说|蓝色/.test(answer.text));
  console.log('[PASS] Live Android text request: ' + answer.model + ', characters=' + answer.text.length + ', fallback=' + answer.fallback_used);
  const picture = await api('/api/mobile-ai/image', 'POST', { prompt: '日系漫画单格，青年读者坐在温暖的旧书店窗边阅读一本蓝色封面的书，木质书架，柔和午后阳光，黑白线稿与少量暖色，无文字，无水印。' });
  const decoded = await evaluate(`(async()=>{const i=new Image();i.src=${JSON.stringify(picture.url)};await i.decode();return {width:i.naturalWidth,height:i.naturalHeight}})()`);
  assert.ok(decoded.width >= 256 && decoded.height >= 256);
  console.log('[PASS] Live Android image request: ' + picture.model + ', ' + decoded.width + 'x' + decoded.height + ', fallback=' + picture.fallback_used);
  const rail = await evaluate('!!document.querySelector("#rail-ai")');
  if (rail) {
    await evaluate('document.querySelector("#rail-ai").click()');
    await new Promise(resolve => setTimeout(resolve, 500));
    await evaluate('document.querySelector(".mai-images")?.scrollIntoView({block:"start"})');
    await new Promise(resolve => setTimeout(resolve, 300));
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    fs.mkdirSync('tests/screenshots', { recursive: true }); fs.writeFileSync('tests/screenshots/android-mobile-ai-live.png', Buffer.from(shot.data, 'base64'));
  }
} finally { socket?.close(); }
