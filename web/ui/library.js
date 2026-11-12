/* ============================================================
   ui/library.js —— 书库页（v0.3 实时刷新版）
   ------------------------------------------------------------
   · 导入 TXT / EPUB / CBZ（拖拽/点选，raw octet-stream + XHR 进度）
   · 书籍列表与进度、格式徽标（TXT/EPUB/CBZ）
   · 实时：EventSource('/api/events') 收到 books_changed → 重新拉
     GET /api/books → 原地 diff 更新（保留 scrollTop、不整页刷新、
     不闪烁，新增卡片淡入高亮一次）
   · 兜底：EventSource 不可用/报错 → 5s 轮询比对；断线自动重连；
     离开书库（pagehide/beforeunload）→ 关闭 EventSource 与定时器
   · 状态点：连接中 / 实时 / 轮询 / 离线
   ============================================================ */
import {
  $, el, clear, store, toast, fmtChars, fmtPercent, apiGet, uploadBook,
} from '../util/core.js';

const EMPTY_TEXT = '书架还是空的 · 把 TXT / EPUB / CBZ 拖到上方导入区即可开始';
const POLL_MS = 5000;          // 契约 8.3：5 秒轮询兜底（SSE 重连也复用该周期，不另开定时器）
const LIVE_TEXT = { connecting: '连接中', live: '实时', polling: '轮询', offline: '离线' };

export async function mountLibrary() {
  const listEl = $('#book-list');
  const emptyEl = $('#lib-empty');
  const drop = $('#drop-zone');
  const input = $('#file-input');
  const status = $('#import-status');
  const liveEl = $('#lib-live');
  const libView = $('#library-view');
  const debug = new URLSearchParams(location.search).get('debug') === '1';

  let importing = false;
  let books = [];              // 最近一次渲染的书目快照
  let fingerprint = '';        // 书目指纹（轮询比对用）
  let destroyed = false;
  let liveState = 'connecting';

  let es = null;               // 任意时刻最多 1 个 EventSource
  let pollTimer = 0;           // 任意时刻最多 1 个轮询定时器（SSE 重连也复用它，不另开定时器）
  let pollKick = 0;            // 进入轮询后尽快补拉一次的短定时器
  let pollInFlight = false;
  let pollFailures = 0;
  let refreshTimer = 0;        // SSE 事件去抖

  /* ---------------- 状态点 ---------------- */
  function setLiveState(state, text) {
    if (!liveEl) return;
    liveState = state;
    liveEl.dataset.state = state;
    const label = liveEl.querySelector('.live-text');
    if (label) label.textContent = text || LIVE_TEXT[state] || state;
    liveEl.title = '书库更新：' + (LIVE_TEXT[state] || state);
  }

  /* ---------------- 数据指纹 / 卡片 ---------------- */
  function fingerprintOf(list) {
    return JSON.stringify(list.map((b) => [
      b.id, b.title, b.format, b.chapter_count, b.page_count, b.total_chars,
      b.last_chapter, Math.round((Number(b.percent) || 0) * 10000),
    ]));
  }

  /* 格式徽标：TXT / EPUB / CBZ（契约 7.3；后端未返回 format 时按 txt 兼容） */
  function fmtInfo(book) {
    const format = String(book.format || 'txt').toLowerCase();
    const label = format === 'epub' ? 'EPUB' : (format === 'cbz' ? 'CBZ' : 'TXT');
    let meta;
    if (format === 'cbz') {
      meta = Number(book.page_count || book.chapter_count || 0) + ' 页 · 漫画';
    } else if (format === 'epub') {
      meta = (book.chapter_count || 0) + ' 章 · EPUB 文本';
    } else {
      meta = (book.chapter_count || 0) + ' 章 · ' + fmtChars(book.total_chars || 0);
    }
    return { format, label, meta };
  }

  function createCard(book) {
    const card = el('a', 'book-card');
    card.dataset.id = book.id;
    const main = el('div', 'book-main');
    const titleRow = el('div', 'book-title-row');
    const badge = el('span', 'fmt-badge');
    const title = el('div', 'book-title');
    titleRow.appendChild(badge);
    titleRow.appendChild(title);
    const meta = el('div', 'book-meta');
    const bar = el('div', 'book-bar');
    const fill = el('div', 'book-bar-fill');
    bar.appendChild(fill);
    main.appendChild(titleRow);
    main.appendChild(meta);
    main.appendChild(bar);
    const side = el('div', 'book-side');
    const state = el('span', 'book-state');
    side.appendChild(state);
    side.appendChild(el('span', 'book-arrow', '→'));
    card.appendChild(main);
    card.appendChild(side);
    card._refs = { badge, title, meta, fill, state, href: '' };
    return card;
  }

  function updateCard(card, book) {
    const refs = card._refs;
    const info = fmtInfo(book);
    const href = '/?book=' + encodeURIComponent(book.id)
      + (Number(book.last_chapter) > 0 ? '&ch=' + book.last_chapter : '');
    if (refs.href !== href) { card.href = href; refs.href = href; }
    const title = book.title || '未命名';
    if (refs.title.textContent !== title) refs.title.textContent = title;
    const badgeText = info.label;
    if (refs.badge.textContent !== badgeText) refs.badge.textContent = badgeText;
    const badgeCls = 'fmt-badge fmt-' + info.format;
    if (refs.badge.className !== badgeCls) refs.badge.className = badgeCls;
    const meta = info.meta + (book.percent > 0 ? ' · 已读 ' + fmtPercent(book.percent) : '');
    if (refs.meta.textContent !== meta) refs.meta.textContent = meta;
    const pct = (Math.max(0, Math.min(1, Number(book.percent) || 0)) * 100).toFixed(1) + '%';
    if (refs.fill.style.width !== pct) refs.fill.style.width = pct;
    const unit = info.format === 'cbz' ? ' 页' : ' 章';
    const stateText = book.percent > 0
      ? '继续阅读 · 第 ' + ((book.last_chapter || 0) + 1) + unit
      : '开始阅读';
    if (refs.state.textContent !== stateText) refs.state.textContent = stateText;
  }

  /* 原地 diff 渲染：复用已有节点，只增删/更新必要部分；保留滚动位置 */
  function applyBooks(next, opts = {}) {
    const list = Array.isArray(next) ? next : [];
    const fp = fingerprintOf(list);
    if (fp === fingerprint && !opts.force) return;
    const wasFirst = !fingerprint;
    fingerprint = fp;
    const prevIds = new Set(books.map((b) => b.id));
    books = list;

    const scrollTop = libView ? libView.scrollTop : 0;
    emptyEl.hidden = list.length > 0;
    if (list.length) {
      if (emptyEl.textContent !== EMPTY_TEXT) emptyEl.textContent = EMPTY_TEXT;
    } else {
      clear(listEl);
      if (libView && scrollTop) libView.scrollTop = 0;
      return;
    }

    const existing = new Map();
    for (const node of Array.from(listEl.children)) {
      const id = node.dataset && node.dataset.id;
      if (id) existing.set(id, node);
    }
    const order = new Set(list.map((b) => b.id));
    const appeared = [];
    const frag = document.createDocumentFragment();
    for (const book of list) {
      let card = existing.get(book.id);
      if (!card) {
        card = createCard(book);
        existing.set(book.id, card);
        if (!wasFirst) appeared.push(card);   // 首次渲染不高亮，后续新增才高亮一次
      }
      updateCard(card, book);
      frag.appendChild(card);
    }
    for (const [id, node] of existing) {
      if (!order.has(id)) node.remove();
    }
    listEl.appendChild(frag);   // 按服务端顺序排列（移动已有节点，不重建）

    // 恢复滚动位置：同步 + 下一帧各一次，避免 reflow 造成跳动
    if (scrollTop) {
      libView.scrollTop = scrollTop;
      requestAnimationFrame(() => { if (!destroyed && libView) libView.scrollTop = scrollTop; });
    }

    // 新增书：淡入 + 高亮一次
    for (const card of appeared) {
      card.classList.remove('is-new');
      void card.offsetWidth;    // 强制重排以重启动画（只在新增时）
      card.classList.add('is-new');
      card.addEventListener('animationend', () => card.classList.remove('is-new'), { once: true });
    }
  }

  async function refreshBooks(opts = {}) {
    if (destroyed) return;
    try {
      const data = await apiGet('/api/books');
      if (destroyed) return;
      applyBooks((data && data.books) || [], { force: !!opts.force });
      if (pollFailures) pollFailures = 0;
    } catch (err) {
      if (destroyed) return;
      if (!books.length) {
        emptyEl.hidden = false;
        emptyEl.textContent = '书架加载失败：' + (err.message || '未知错误');
      }
      if (opts.manual) toast('书架刷新失败：' + (err.message || '未知错误'));
      if (opts.poll) {
        pollFailures += 1;
        if (pollFailures >= 2) setLiveState('offline');
      }
    }
  }

  /* ---------------- 5s 轮询兜底 ---------------- */
  function startPolling() {
    if (pollTimer || destroyed) return;
    setLiveState('polling');
    pollTimer = setInterval(pollOnce, POLL_MS);
    if (pollKick) clearTimeout(pollKick);
    pollKick = setTimeout(() => { pollKick = 0; if (!destroyed && pollTimer) pollOnce(); }, 300);  // 尽快补一次
  }

  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = 0; }
    if (pollKick) { clearTimeout(pollKick); pollKick = 0; }
  }

  async function pollOnce() {
    if (destroyed || pollInFlight) return;
    pollInFlight = true;
    try {
      const data = await apiGet('/api/books');
      pollFailures = 0;
      if (destroyed) return;
      applyBooks((data && data.books) || []);
      if (es && es.readyState === 1) setLiveState('live');
      else if (liveState === 'offline') setLiveState('polling');
    } catch (err) {
      pollFailures += 1;
      if (pollFailures >= 2) setLiveState('offline');
    } finally {
      pollInFlight = false;
      // 轮询期间每个周期都尝试一次 SSE 重连（仍是同一个定时器）；成功则 open 事件里停掉轮询
      if (!es) openEventSource();
    }
  }

  /* ---------------- EventSource（SSE） ---------------- */
  function scheduleRefresh(delay) {
    if (destroyed) return;
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = 0;
      refreshBooks();
    }, delay || 120);
  }

  function closeEventSource() {
    if (!es) return;
    try { es.close(); } catch (e) { /* ignore */ }
    es = null;
  }

  function openEventSource() {
    if (destroyed || es) return;
    if (typeof EventSource === 'undefined') {
      startPolling();          // 浏览器不支持 SSE → 轮询
      return;
    }
    let source;
    try {
      source = new EventSource('/api/events');
    } catch (e) {
      startPolling();
      return;
    }
    es = source;
    if (!pollTimer) setLiveState('connecting');   // 已处于轮询/离线时，重连尝试不覆盖状态（避免状态闪烁）
    source.addEventListener('open', () => {
      if (destroyed) return;
      stopPolling();
      pollFailures = 0;
      setLiveState('live');
    });
    source.addEventListener('books_changed', () => {
      if (destroyed) return;
      if (liveState !== 'live') setLiveState('live');
      scheduleRefresh(100);    // 多事件合并成一次拉取
    });
    source.addEventListener('error', () => {
      if (destroyed || !es) return;
      if (source.readyState === 2) {
        // CLOSED：不可恢复（404/非 SSE 响应等）→ 轮询（轮询周期内自动尝试重连）
        closeEventSource();
        startPolling();
        if (pollFailures >= 2) setLiveState('offline');
      } else {
        // CONNECTING：浏览器会自动重连，期间用轮询兜底，避免长时间无更新
        startPolling();
        setLiveState(pollFailures >= 2 ? 'offline' : 'polling');
      }
    });
  }

  /* ---------------- 生命周期 ---------------- */
  function destroy() {
    if (destroyed) return;
    destroyed = true;
    closeEventSource();
    stopPolling();
    if (refreshTimer) { clearTimeout(refreshTimer); refreshTimer = 0; }
    pollInFlight = false;
  }
  window.addEventListener('pagehide', destroy, { once: true });
  window.addEventListener('beforeunload', destroy, { once: true });
  // bfcache 恢复（后退回书库）：重新订阅并补拉一次，避免停在已销毁状态
  window.addEventListener('pageshow', (e) => {
    if (!e.persisted || !destroyed) return;
    destroyed = false;
    setLiveState('connecting');
    refreshBooks({ force: true });
    openEventSource();
  });

  /* ---------------- 导入 ---------------- */
  async function importFile(file) {
    if (importing || !file) return;
    if (!/\.(txt|epub|cbz|zip)$/i.test(file.name || '')) { toast('支持 TXT / EPUB / CBZ 文件'); return; }
    if (file.size === 0) { toast('文件是空的'); return; }
    if (file.size > 64 * 1024 * 1024) { toast('文件超过 64MB 上限'); return; }
    importing = true;
    setImporting(0, file.name);
    try {
      const res = await uploadBook(file, (p) => setImporting(p, file.name));
      store.set({ book: res.book.id, chapter: 0, para: 0, offset: 0 });
      toast(res.deduped ? '这本书已在书架中' : '导入成功 · ' + res.book.chapter_count + ' 章');
      setImporting(1, file.name);
      setTimeout(() => {
        location.href = '/?book=' + encodeURIComponent(res.book.id);
      }, 320);
    } catch (err) {
      importing = false;
      setImporting(-1);
      toast('导入失败：' + (err.message || '未知错误'));
    }
  }

  function setImporting(progress, name) {
    if (progress < 0) {
      status.hidden = true;
      drop.classList.remove('busy');
      return;
    }
    drop.classList.add('busy');
    status.hidden = false;
    status.textContent = progress >= 1
      ? '解析完成，正在进入阅读…'
      : '上传中 ' + (name || '') + ' · ' + Math.round(progress * 100) + '%';
  }

  /* ---------------- 事件 ---------------- */
  input.addEventListener('change', () => {
    importFile(input.files && input.files[0]);
    input.value = '';
  });
  drop.addEventListener('click', () => { if (!importing) input.click(); });
  drop.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); if (!importing) input.click(); }
  });
  let dragDepth = 0;
  drop.addEventListener('dragenter', (e) => { e.preventDefault(); dragDepth += 1; drop.classList.add('drag'); });
  drop.addEventListener('dragover', (e) => { e.preventDefault(); });
  drop.addEventListener('dragleave', () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth) drop.classList.remove('drag'); });
  drop.addEventListener('drop', (e) => {
    e.preventDefault();
    dragDepth = 0;
    drop.classList.remove('drag');
    const file = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
    importFile(file);
  });
  $('#lib-refresh').addEventListener('click', () => refreshBooks({ manual: true, force: true }));

  /* ---------------- 启动 ---------------- */
  setLiveState('connecting');
  await refreshBooks({ force: true });
  openEventSource();
  setLiveState(es ? 'connecting' : (pollTimer ? 'polling' : 'connecting'));

  if (debug) {
    window.__lib = {
      state: () => liveState,
      books: () => books.slice(),
      esActive: () => !!es,
      esReadyState: () => (es ? es.readyState : -1),
      polling: () => !!pollTimer,
      destroy,
      refresh: () => refreshBooks({ manual: true, force: true }),
      fingerprint: () => fingerprint,
    };
  }

  return { destroy };
}
