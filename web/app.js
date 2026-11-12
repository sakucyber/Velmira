/* ============================================================
   app.js —— 入口路由（无构建，原生 ESM）
   /                  → 书库
   /?book=<id>&ch=&p=&para=  → 阅读器
   主题在 index.html 内联脚本里已尽早应用，这里只做路由分发。
   ============================================================ */
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
