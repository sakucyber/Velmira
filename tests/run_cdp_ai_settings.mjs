// Run the settings UI regression without using any existing book library.
import { spawn } from 'node:child_process';
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
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'velmira-settings-e2e-'));
const servicePort = await freePort(), debugPort = await freePort();
const base = 'http://127.0.0.1:' + servicePort;
let service, test;
try {
  service = spawn(process.env.PYTHON || 'python', ['-m', 'uvicorn', 'server.app.main:app', '--host', '127.0.0.1', '--port', String(servicePort), '--no-proxy-headers'], {
    cwd: path.resolve('.'), env: { ...process.env, DATA_DIR: path.join(temporary, 'data') }, stdio: 'ignore', windowsHide: true,
  });
  let ready = false;
  for (let index = 0; index < 100; index++) {
    try { if ((await fetch(base + '/api/health')).ok) { ready = true; break; } } catch {}
    if (service.exitCode !== null) break; await sleep(100);
  }
  assert.ok(ready, 'Disposable settings service starts');
  test = spawn(process.execPath, ['tests/cdp_ai_settings.mjs', '--base', base], {
    cwd: path.resolve('.'), env: { ...process.env, CDP_PORT: String(debugPort) }, stdio: 'inherit', windowsHide: true,
  });
  const [code] = await once(test, 'exit');
  if (code !== 0) process.exitCode = code || 1;
} finally {
  for (const child of [test, service]) if (child && child.exitCode === null) {
    if (process.platform === 'win32') { const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true }); await once(killer, 'exit'); }
    else { child.kill(); await once(child, 'exit'); }
  }
  const target = path.resolve(temporary), parent = path.resolve(os.tmpdir()) + path.sep;
  assert.ok(target.startsWith(parent) && path.basename(target).startsWith('velmira-settings-e2e-'), 'Cleanup stays inside the owned temporary directory');
  await fs.promises.rm(target, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
