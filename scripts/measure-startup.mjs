// Isolated Windows desktop startup benchmark; no access to the user's library.
// node scripts/measure-startup.mjs --exe desktop/Velmira.exe
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { once } from 'node:events';
import assert from 'node:assert/strict';
const args = process.argv.slice(2);
const exe = path.resolve(args.includes('--exe') ? args[args.indexOf('--exe') + 1] : 'desktop/Velmira.exe');
const scenario = args.includes('--scenario') ? args[args.indexOf('--scenario') + 1] : 'normal';
assert.ok(['normal', 'early-close', 'failure'].includes(scenario));
const root = path.resolve('.');
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function port() {
  const server = net.createServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const value = server.address().port; await new Promise(r => server.close(r)); return value;
}
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-startup-'));
const backendPort = await port(), debugPort = await port();
if (scenario !== 'normal') fs.writeFileSync(path.join(temp, 'uvicorn.py'), `import os,time\nfrom pathlib import Path\nPath(os.environ['DATA_DIR']).mkdir(parents=True, exist_ok=True)\nPath(os.environ['DATA_DIR'], 'pid').write_text(str(os.getpid()))\ntime.sleep(${scenario === 'failure' ? 1 : 30})\nraise SystemExit(7)\n`);
const started = performance.now();
let ws;
const child = spawn(exe, ['-port', String(backendPort), '-screenshot', path.join(temp, 'window.png'), '-wait', '3s', ...(scenario !== 'normal' ? ['-python', 'python'] : [])], {
  cwd: root, stdio: 'ignore', env: { ...process.env, DATA_DIR: path.join(temp, 'data'),
    WEBVIEW2_USER_DATA_FOLDER: path.join(temp, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort}`,
    ...(scenario !== 'normal' ? { PYTHONPATH: temp } : {}) },
});
const exited = once(child, 'exit');
let sequence = 0;
const pending = new Map();
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, method, params })); });
}
try {
  let target;
  while (performance.now() - started < 20000 && child.exitCode === null) {
    try {
      const r = await fetch(`http://127.0.0.1:${debugPort}/json`, { signal: AbortSignal.timeout(100) });
      target = (await r.json()).find(t => t.type === 'page'); if (target) break;
    } catch {}
    await sleep(20);
  }
  if (!target) throw new Error('Desktop WebView did not start');
  const webviewMs = Math.round(performance.now() - started);
  ws = new WebSocket(target.webSocketDebuggerUrl); await new Promise((r, reject) => { ws.onopen = r; ws.onerror = reject; });
  ws.onmessage = e => { const m = JSON.parse(e.data); const p = pending.get(m.id); if (p) { pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); } };
  let ready = false;
  const expression = scenario === 'normal'
    ? `!!document.querySelector('#library-view:not([hidden])') && document.querySelector('#lib-skeleton')?.hidden && !document.querySelector('#lib-error:not([hidden])') && (document.querySelector('#lib-empty:not([hidden])') !== null || document.querySelector('.book-card') !== null)`
    : scenario === 'failure' ? `document.body?.textContent.includes('后台服务提前退出')` : `document.body?.textContent.includes('正在打开书架')`;
  while (performance.now() - started < 20000) {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true });
    if (r.result.value) { ready = true; break; }
    await sleep(20);
  }
  if (!ready) throw new Error('Expected desktop state never appeared: ' + scenario);
  console.log(JSON.stringify({ exe, scenario, webviewMs, readyMs: Math.round(performance.now() - started) }));
  if (args.includes('--screenshot')) {
    await sleep(100);
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(path.resolve(args[args.indexOf('--screenshot') + 1]), Buffer.from(shot.data, 'base64'));
  }
  if (scenario !== 'normal') {
    // Wait for a native window handle; the debugger can connect before first paint.
    const closer = spawn('powershell', ['-NoProfile', '-Command', `$p=[Diagnostics.Process]::GetProcessById(${child.pid}); for($i=0;$i -lt 40;$i++){ $p.Refresh(); if($p.MainWindowHandle -ne 0){ if($p.CloseMainWindow()){exit 0}; exit 2 }; Start-Sleep -Milliseconds 50 }; exit 3`], { windowsHide: true, stdio: 'ignore' });
    await once(closer, 'exit');
    assert.equal(closer.exitCode, 0, 'Native window close was requested');
  }
  let timeout;
  try { await Promise.race([exited, new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error('Desktop failed to exit')), 10000); })]); }
  finally { clearTimeout(timeout); }
  if (scenario !== 'normal') {
    const pid = Number(fs.readFileSync(path.join(temp, 'data', 'pid'), 'utf8'));
    let alive = false; try { process.kill(pid, 0); alive = true; } catch {}
    assert.equal(alive, false, 'Owned backend must exit with the window');
    console.log('[PASS] ' + scenario + ': window closed; no backend process remains');
  }
} finally {
  ws?.close();
  if (child.exitCode === null) {
    const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' }); await once(killer, 'exit');
  }
  await sleep(500); // WebView child processes release profile handles after the window exits.
  await fs.promises.rm(temp, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
