/* ============================================================
   ui/comic.js —— CBZ 漫画阅读模式（契约 7.4 / 7.6）
   入口：export async function mountComic({ bookId, params, book })
   - 单 <img> 换 src（不预加载、不用离屏绘制或对象 URL）
   - 页面元数据 GET /pages 懒拉，仅保留当前页 ±1
   - 进度 GET/PUT /progress（chapter=页码, para=0），≥800ms 防抖
   - 键盘 ←/→/空格、上下页按钮、左右点击区、触摸滑动 40px
   - 适页 / 适宽（localStorage 记忆）；?debug=1 显示 heap/DOM/img
   ============================================================ */
import { el, debounce, clamp, intOrNull, apiGet, toast } from '../util/core.js';

const FIT_KEY = 'velmira.comic.fit';
const SAVE_MS = 900;      /* 约束：≥800ms，禁止每页都 PUT */
const SWIPE_PX = 40;      /* 触摸滑动阈值 */
const CSS_HREF = new URL('../styles/comic.css', import.meta.url).href;

function readFit() {
  try {
    const raw = localStorage.getItem(FIT_KEY);
    const obj = raw ? JSON.parse(raw) : null;
    return obj && obj.fit === 'width' ? 'width' : 'page';
  } catch (e) { return 'page'; }
}
function saveFit(fit) {
  try { localStorage.setItem(FIT_KEY, JSON.stringify({ fit: fit })); } catch (e) { /* 隐私模式静默 */ }
}
function ensureStyles() {
  if (document.querySelector('link[data-comic-styles]')) return;
  const link = document.createElement('link');
  link.rel = 'stylesheet';
  link.href = CSS_HREF;
  link.dataset.comicStyles = '1';
  document.head.appendChild(link);
}
function comicFatal(main, msg) {
  const box = el('div', 'fatal');
  box.appendChild(el('p', 'fatal-msg', msg));
  const back = el('a', 'btn', '返回书架');
  back.href = '/';
  box.appendChild(back);
  main.replaceChildren(box);
}

export async function mountComic(opts = {}) {
  const bookId = opts.bookId;
  const params = opts.params instanceof URLSearchParams ? opts.params : new URLSearchParams(location.search);
  const debug = params.get('debug') === '1' || new URLSearchParams(location.search).get('debug') === '1';
  const main = document.getElementById('reader-main');
  if (!main || !bookId) return null;

  ensureStyles();
  const enc = encodeURIComponent(bookId);

  let book = opts.book || null;
  if (!book) {
    try { book = (await apiGet('/api/books/' + enc)).book; }
    catch (err) { comicFatal(main, '书目加载失败，请返回书架重试'); return null; }
  }
  const count = Math.max(1, Number(book.page_count) || Number(book.chapter_count) || 1);

  /* ================== 骨架（约 30 节点，无列表缓存） ================== */
  const root = el('div', 'comic-root');

  const back = el('a', 'comic-back', '←');
  back.href = '/';
  back.title = '返回书架';
  back.setAttribute('aria-label', '返回书架');

  const title = el('div', 'comic-title', book.title || '未命名');
  const countLabel = el('div', 'comic-count', '— / —');
  const meta = el('div', 'comic-meta');
  meta.append(title, countLabel);

  const jumpInput = el('input', 'comic-jump-input');
  jumpInput.type = 'number';
  jumpInput.min = '1';
  jumpInput.max = String(count);
  jumpInput.inputMode = 'numeric';
  jumpInput.setAttribute('aria-label', '跳转页码');
  const jumpBtn = el('button', 'comic-jump-btn', '跳');
  jumpBtn.type = 'submit';
  const jump = el('form', 'comic-jump');
  jump.append(jumpInput, jumpBtn);

  const fitBtn = el('button', 'comic-fit', '适页');
  fitBtn.type = 'button';
  fitBtn.title = '切换适宽 / 适页';

  const tools = el('div', 'comic-tools');
  tools.append(jump, fitBtn);
  const top = el('header', 'comic-top');
  top.append(back, meta, tools);

  const img = el('img', 'comic-img');
  img.alt = '';
  img.decoding = 'async';
  img.draggable = false;

  const spinner = el('div', 'comic-spinner');
  const loading = el('div', 'comic-loading');
  loading.appendChild(spinner);

  const errorMsg = el('p', 'comic-error-msg', '图片加载失败');
  const retryBtn = el('button', 'comic-retry', '重试');
  retryBtn.type = 'button';
  const errorBox = el('div', 'comic-error');
  errorBox.append(errorMsg, retryBtn);
  errorBox.hidden = true;

  const prevBtn = el('button', 'comic-arrow comic-prev', '‹');
  prevBtn.type = 'button';
  prevBtn.setAttribute('aria-label', '上一页');
  const nextBtn = el('button', 'comic-arrow comic-next', '›');
  nextBtn.type = 'button';
  nextBtn.setAttribute('aria-label', '下一页');

  const stage = el('div', 'comic-stage');
  stage.append(img, loading, errorBox);

  const fill = el('div', 'comic-fill');
  const track = el('div', 'comic-track');
  track.setAttribute('role', 'progressbar');
  track.setAttribute('aria-label', '阅读进度');
  track.setAttribute('aria-valuemin', '0');
  track.setAttribute('aria-valuemax', '100');
  track.appendChild(fill);
  const pageLabel = el('span', 'comic-page', '— / —');
  const foot = el('footer', 'comic-foot');
  foot.append(track, pageLabel);

  root.append(top, stage, foot, prevBtn, nextBtn);
  main.replaceChildren(root);
  document.body.classList.add('comic-mode');

  /* ================== 状态 ================== */
  let page = 0;
  let percent = 0;
  let fit = readFit();
  let loadToken = 0;
  let expectSrc = '';
  let loadStart = 0;
  let lastLoadMs = 0;
  let metaAc = null;
  let swipeSuppressClickUntil = 0;
  let debugEl = null;
  let debugTimer = 0;
  const metaCache = new Map();   /* 只保留当前页 ±1 的元数据 */

  const scheduleSave = debounce(saveProgress, SAVE_MS);

  /* ================== 页面元数据（懒拉 + 只留 ±1） ================== */
  async function ensureMeta(idx) {
    if (metaCache.has(idx)) return metaCache.get(idx);
    if (metaAc) metaAc.abort();
    const ac = new AbortController();
    metaAc = ac;
    const from = Math.max(0, idx - 1);
    const data = await apiGet('/api/books/' + enc + '/pages?from=' + from + '&limit=3', ac.signal);
    const list = (data && data.pages) || [];
    for (let i = 0; i < list.length; i++) {
      const p = list[i];
      if (p && Number.isFinite(Number(p.idx))) metaCache.set(Number(p.idx), p);
    }
    /* 硬约束：不保留当前页 ±1 之外的元数据 */
    for (const key of Array.from(metaCache.keys())) {
      if (key < idx - 1 || key > idx + 1) metaCache.delete(key);
    }
    return metaCache.get(idx) || null;
  }

  /* ================== 单图换 src（唯一 <img>） ================== */
  function setLoading(on) { loading.hidden = !on; }

  function loadImage(url, bust) {
    const abs = new URL(url, location.href).href;
    expectSrc = bust ? abs + (abs.indexOf('?') >= 0 ? '&' : '?') + 'r=' + Date.now() : abs;
    errorBox.hidden = true;
    setLoading(true);
    loadStart = performance.now();
    img.src = expectSrc;   /* 只改 src：不新建元素、不缓存二进制 */
  }

  img.addEventListener('load', () => {
    if (img.src !== expectSrc) return;   /* 已被后一次换页取代 */
    if (loadStart) { lastLoadMs = performance.now() - loadStart; loadStart = 0; }
    setLoading(false);
    errorBox.hidden = true;
    if (debug) updateDebug();
  });
  img.addEventListener('error', () => {
    if (!img.getAttribute('src') || img.src !== expectSrc) return;
    setLoading(false);
    showError('图片加载失败，请重试');
  });

  function showError(msg) {
    setLoading(false);
    errorMsg.textContent = msg;
    errorBox.hidden = false;
  }

  retryBtn.addEventListener('click', () => {
    const m = metaCache.get(page);
    if (m && m.url) loadImage(m.url, true);   /* 仅重试时加 cache-bust，正常加载保持 immutable 缓存 */
    else showPage(page);
  });

  /* ================== 页面切换 ================== */
  function updateChrome() {
    const label = '第 ' + (page + 1) + ' / ' + count + ' 页';
    countLabel.textContent = (page + 1) + ' / ' + count + ' 页';
    pageLabel.textContent = label;
    const pct = clamp(percent, 0, 1) * 100;
    fill.style.width = pct.toFixed(2) + '%';
    track.setAttribute('aria-valuenow', String(Math.round(pct)));
    if (document.activeElement !== jumpInput) jumpInput.value = String(page + 1);
    document.title = (book.title || '漫画') + ' · Velmira';
    syncUrl();
  }

  function syncUrl() {
    const url = location.pathname + '?book=' + enc + '&ch=' + page + (debug ? '&debug=1' : '');
    try { history.replaceState({ book: bookId, ch: page }, '', url); } catch (e) { /* ignore */ }
  }

  async function showPage(idx, save) {
    const next = clamp(Number(idx) || 0, 0, count - 1);
    const token = ++loadToken;
    page = next;
    percent = (page + 1) / count;
    errorBox.hidden = true;
    updateChrome();
    let m;
    try {
      m = await ensureMeta(page);
    } catch (err) {
      if (err && err.name === 'AbortError') return;
      if (token !== loadToken) return;
      showError('页面信息加载失败，请重试');
      if (save !== false) scheduleSave();
      return;
    }
    if (token !== loadToken) return;
    if (!m || !m.url) {
      showError('第 ' + (page + 1) + ' 页不存在');
      if (save !== false) scheduleSave();
      return;
    }
    loadImage(m.url, false);
    if (save !== false) scheduleSave();
  }

  function go(idx) {
    if (idx < 0) { toast('已经是第一页了'); return; }
    if (idx >= count) { toast('已经是最后一页了'); return; }
    if (idx === page) return;
    showPage(idx);
  }

  /* ================== 进度（GET 恢复 / PUT 防抖） ================== */
  async function saveProgress() {
    const at = page;
    try {
      const res = await fetch('/api/books/' + enc + '/progress', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ chapter: at, para: 0, offset: 0 }),
        cache: 'no-store',
        keepalive: true,
      });
      if (!res.ok) return;
      const data = await res.json().catch(() => null);
      const p = data && data.progress ? Number(data.progress.percent) : NaN;
      if (Number.isFinite(p) && page === at) {   /* 页码未变才采用服务端 percent */
        percent = clamp(p, 0, 1);
        updateChrome();
      }
    } catch (e) { /* 进度保存失败静默，不阻塞阅读 */ }
  }

  /* ================== 交互：键盘 / 点击区 / 按钮 / 触摸 / 跳转 ================== */
  function onKeydown(e) {
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.key === 'ArrowLeft' || e.key === 'PageUp') { e.preventDefault(); go(page - 1); }
    else if (e.key === 'ArrowRight' || e.key === 'PageDown' || e.key === ' ') { e.preventDefault(); go(page + 1); }
  }
  document.addEventListener('keydown', onKeydown);

  function onStageClick(e) {
    if (Date.now() < swipeSuppressClickUntil) return;
    if (e.target.closest('button, input, form, a, .comic-error')) return;
    const rect = stage.getBoundingClientRect();
    const ratio = (e.clientX - rect.left) / Math.max(1, rect.width);
    if (ratio < 1 / 3) go(page - 1);
    else if (ratio > 2 / 3) go(page + 1);
  }
  stage.addEventListener('click', onStageClick);

  let pointerStart = null;
  stage.addEventListener('pointerdown', (e) => {
    if (e.target.closest('button, input, form, a, .comic-error')) { pointerStart = null; return; }
    pointerStart = { x: e.clientX, y: e.clientY, id: e.pointerId, t: Date.now() };
  });
  stage.addEventListener('pointerup', (e) => {
    if (!pointerStart || e.pointerId !== pointerStart.id) return;
    const dx = e.clientX - pointerStart.x;
    const dy = e.clientY - pointerStart.y;
    pointerStart = null;
    if (Math.abs(dx) < SWIPE_PX || Math.abs(dx) <= Math.abs(dy)) return;
    swipeSuppressClickUntil = Date.now() + 500;   /* 滑动后的合成 click 不再触发点击区翻页 */
    if (dx < 0) go(page + 1); else go(page - 1);
  });
  stage.addEventListener('pointercancel', () => { pointerStart = null; });

  prevBtn.addEventListener('click', () => go(page - 1));
  nextBtn.addEventListener('click', () => go(page + 1));

  jump.addEventListener('submit', (e) => {
    e.preventDefault();
    const n = intOrNull(jumpInput.value);
    if (n === null) { jumpInput.value = String(page + 1); return; }
    jumpInput.blur();
    go(clamp(n, 1, count) - 1);
  });
  jumpInput.addEventListener('blur', () => { jumpInput.value = String(page + 1); });

  /* ================== 适宽 / 适页 ================== */
  function applyFit(next) {
    fit = next === 'width' ? 'width' : 'page';
    root.classList.toggle('comic-fit-width', fit === 'width');
    root.classList.toggle('comic-fit-page', fit === 'page');
    fitBtn.textContent = fit === 'width' ? '适宽' : '适页';
    fitBtn.setAttribute('aria-label', fit === 'width' ? '当前适宽，点击切换适页' : '当前适页，点击切换适宽');
  }
  fitBtn.addEventListener('click', () => { saveFit(fit === 'width' ? 'page' : 'width'); applyFit(fit === 'width' ? 'page' : 'width'); });
  applyFit(fit);

  /* ================== debug（?debug=1，右下角 heap/DOM/img） ================== */
  function heapMB() {
    try { return performance.memory ? performance.memory.usedJSHeapSize / 1048576 : NaN; } catch (e) { return NaN; }
  }
  function updateDebug() {
    if (!debugEl) return;
    const heap = heapMB();
    const imgCount = document.querySelectorAll('img').length;
    const nodes = document.querySelectorAll('*').length;
    debugEl.textContent = (Number.isFinite(heap) ? heap.toFixed(2) + 'MB' : '?MB')
      + ' · ' + nodes + ' DOM · ' + imgCount + ' img'
      + ' · p' + (page + 1) + '/' + count
      + (lastLoadMs ? ' · ' + Math.round(lastLoadMs) + 'ms' : '')
      + ' · ' + metaCache.size + ' meta';
  }
  if (debug) {
    debugEl = el('div', 'comic-debug');
    root.appendChild(debugEl);
    updateDebug();
    debugTimer = setInterval(updateDebug, 1200);
    window.__comic = {
      go: go,
      page: () => page,
      count: count,
      fit: () => fit,
      nodes: () => document.querySelectorAll('*').length,
      heap: () => heapMB(),
      imgCount: () => document.querySelectorAll('img').length,
      metaKeys: () => Array.from(metaCache.keys()),
      lastLoadMs: () => lastLoadMs,
    };
  }

  /* ================== 初始页：?ch 优先，否则 GET 进度 ================== */
  let start = intOrNull(params.get('ch'));
  if (start === null) {
    try {
      const pr = (await apiGet('/api/books/' + enc + '/progress')).progress;
      start = Number(pr && pr.chapter) || 0;
      if (pr && Number.isFinite(Number(pr.percent))) percent = clamp(Number(pr.percent), 0, 1);
    } catch (e) { start = 0; }
  }
  await showPage(clamp(Number(start) || 0, 0, count - 1), false);
  updateChrome();

  /* ================== 离开页面：flush 进度、清理计时器 ================== */
  window.addEventListener('pagehide', () => {
    scheduleSave.flush();
    if (metaAc) metaAc.abort();
    if (debugTimer) clearInterval(debugTimer);
    document.removeEventListener('keydown', onKeydown);
  }, { once: true });

  return {
    get page() { return page; },
    count: count,
    go: go,
    next: () => go(page + 1),
    prev: () => go(page - 1),
  };
}
