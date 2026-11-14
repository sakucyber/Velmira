/* ============================================================
   app.js —— 入口路由（无构建，原生 ESM）
   /                  → 书库
   /?book=<id>&ch=&p=&para=  → 阅读器
   主题在 index.html 内联脚本里已尽早应用，这里只做路由分发。
   ============================================================ */
import { bootstrapAuth, getAuthState } from './util/core.js';
import { localStore } from './util/local-store.js';

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('./sw.js', { scope: './' }).catch(() => {});
}

window.addEventListener('velmira:authentication-required', () => location.reload(), { once: true });

if (await bootstrapAuth()) {
if (getAuthState().standalone) {
  const { showOnboarding } = await import('./ui/onboarding.js');
  await showOnboarding();
}
let syncBusy = false;
async function autoSync() {
  if (getAuthState().standalone || syncBusy || !navigator.onLine || document.hidden || (await localStore.meta('auto-sync')) === false) return;
  syncBusy = true;
  try { await localStore.sync(); window.dispatchEvent(new Event('velmira:synced')); }
  catch { /* Edits stay on the device until the next successful sync. */ }
  finally { syncBusy = false; }
}
window.addEventListener('online', autoSync);
document.addEventListener('visibilitychange', () => { if (!document.hidden) autoSync(); });
const syncTimer = setInterval(autoSync, 60000);
window.addEventListener('pagehide', () => clearInterval(syncTimer), { once: true });
autoSync();
const params = new URLSearchParams(location.search);
const bookId = params.get('book');
const debug = params.get('debug') === '1';

const libView = document.getElementById('library-view');
const readerView = document.getElementById('reader-view');

if (debug) document.documentElement.classList.add('debug');

if (bookId) {
  libView.hidden = true;
  readerView.hidden = false;
  const { mountReader } = await import('./ui/reader.js');
  try {
    await mountReader({ bookId, params, debug });
  } catch (err) {
    console.error('[velmira] reader mount failed', err);
    const main = document.getElementById('reader-main');
    if (main) {
      main.replaceChildren();
      const box = document.createElement('div');
      box.className = 'fatal';
      const p = document.createElement('p');
      p.className = 'fatal-msg';
      p.textContent = '阅读器加载失败：' + ((err && err.message) || '未知错误');
      const back = document.createElement('a');
      back.className = 'btn';
      back.href = '/';
      back.textContent = '返回书架';
      box.append(p, back);
      main.appendChild(box);
    }
  }
} else {
  readerView.hidden = true;
  libView.hidden = false;
  const { mountLibrary } = await import('./ui/library.js');
  await mountLibrary();
}
}
