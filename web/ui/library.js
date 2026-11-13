/* ============================================================
   ui/library.js —— 书库页（Velmira 桌面书架 · v0.3 实时刷新）
   ------------------------------------------------------------
   · 导入 TXT / EPUB / CBZ（拖拽/点选，raw octet-stream + XHR 进度）
   · 书架封面网格（格式纸感封面 / 标题首字 / 进度 / 元信息）
   · 书名过滤 + 「书架 / 继续阅读」视图切换（纯前端，不影响后端搜索）
   · 实时：EventSource('/api/events') 收到 books_changed → GET /api/books
     → 原地 diff 更新（保留 scrollTop、不整页刷新、新增卡片入场一次）
   · 兜底：SSE 不可用/报错 → 5s 轮询；断线自动重连；pagehide 清理
   · 状态点：连接中 / 实时 / 轮询 / 离线；骨架屏 / 行内错误条
   ============================================================ */
import {
  $, el, store, toast, fmtChars, apiGet, uploadBook,
  THEMES, FONT_SIZE, applyTheme, applyFontSize,
} from '../util/core.js';
import { readingState, readingLabel, coverHue, shelfPreferences, saveShelfPreferences, openBookDetails } from './library-tools.js';

const EMPTY_TEXT = '拖入或选择 TXT / EPUB / CBZ 文件，导入后在这里开始阅读。';
const POLL_MS = 5000;          // 契约 8.3：5 秒轮询兜底（SSE 重连也复用该周期）
const LIVE_TEXT = { connecting: '连接中', live: '自动更新', polling: '定时更新', offline: '离线' };
const FORMAT_LABEL = { txt: 'TXT', epub: 'EPUB', cbz: 'CBZ' };

export async function mountLibrary() {
  const listEl = $('#book-list');
  const emptyEl = $('#lib-empty');
  const emptyTextEl = $('#lib-empty-text');
  const noresultEl = $('#lib-noresult');
  const skeletonEl = $('#lib-skeleton');
  const errorEl = $('#lib-error');
  const errorTextEl = $('#lib-error-text');
  const drop = $('#drop-zone');
  const input = $('#file-input');
  const status = $('#import-status');
  const liveEl = $('#lib-live');
  const libView = $('#library-view');
  const dropOverlay = $('#library-drop-overlay');
  const appMain = $('.app-main');
  const filterInput = $('#lib-filter');
  const countEls = [$('#lib-count'), $('#lib-sub-count')];
  const shelfCountEl = $('#shelf-count');
  const shelfTitleEl = $('#shelf-title');
  const shelfHeadEl = $('.shelf-head');
  const headerEl = $('.lib-header');
  const navLibrary = $('#nav-library');
  const navContinue = $('#nav-continue');
  const debug = new URLSearchParams(location.search).get('debug') === '1';
  const formatFilter = $('#lib-format');
  const statusFilter = $('#lib-status');
  const sortSelect = $('#lib-sort');
  const clearSearch = $('#lib-clear');
  const prefs = shelfPreferences();
  let density = prefs.density === 'compact' ? 'compact' : 'standard';
  sortSelect.value = ['added', 'read', 'title', 'progress'].includes(prefs.sort) ? prefs.sort : 'added';
  let archivedBooks = [];
  let archivedRequest = 0;

  let importing = false;
  let books = [];              // 最近一次书目快照（全量）
  let fingerprint = '';        // 书目指纹（轮询比对用）
  let destroyed = false;
  let liveState = 'connecting';
  let loadedOnce = false;
  let firstEntryPlayed = false;
  let viewMode = 'all';        // 'all' | 'reading'
  let filterText = '';

  let es = null;               // 任意时刻最多 1 个 EventSource
  let pollTimer = 0;           // 任意时刻最多 1 个轮询定时器
  let pollKick = 0;
  let pollInFlight = false;
  let pollFailures = 0;
  let refreshTimer = 0;        // SSE 事件去抖
  let settingsPanels = null;

  async function openSettings() {
    const button = $('#rail-settings');
    if (button.disabled) return;
    button.disabled = true;
    try {
      if (!settingsPanels) {
        const { createPanels } = await import('./panels.js');
        if (destroyed) return;
        settingsPanels = createPanels({ layer: $('#drawer-layer') });
      }
      const saved = store.get();
      settingsPanels.openSettings({
        themes: THEMES, theme: THEMES.some(t => t.id === saved.theme) ? saved.theme : 'day',
        fontRange: FONT_SIZE, fontSize: applyFontSize(saved.fontSize),
        books: books.filter(book => book.format !== 'cbz'),
        onTheme: (theme) => { applyTheme(theme); store.set({ theme }); },
        onFont: async (value) => { store.set({ fontSize: applyFontSize(value) }); },
      });
    } catch (err) { toast('设置加载失败：' + err.message); }
    finally { button.disabled = false; }
  }

  /* ---------------- 状态点 ---------------- */
  function setLiveState(state, text) {
    if (!liveEl) return;
    liveState = state;
    liveEl.dataset.state = state;
    const label = liveEl.querySelector('.live-text');
    if (label) label.textContent = text || LIVE_TEXT[state] || state;
    liveEl.title = '书库内容变化会自动刷新列表；连接中断时每 5 秒重试。不监视电脑文件夹。当前：' + (LIVE_TEXT[state] || state);
  }

  /* ---------------- 数据指纹 / 元信息 ---------------- */
  function fingerprintOf(list) {
    return JSON.stringify(list.map((b) => [
      b.id, b.title, b.format, b.chapter_count, b.page_count, b.total_chars,
      b.last_chapter, b.has_progress, b.last_read_at, Math.round((Number(b.percent) || 0) * 1000000),
    ]));
  }

  function fmtCount(n) {
    n = Number(n) || 0;
    return n >= 10000 ? fmtChars(n) : n.toLocaleString('en-US') + ' 字';
  }

  function fmtInfo(book) {
    const format = String(book.format || 'txt').toLowerCase();
    const label = FORMAT_LABEL[format] || 'TXT';
    let meta;
    if (format === 'cbz') {
      meta = Number(book.page_count || book.chapter_count || 0) + ' 页 · 漫画';
    } else {
      meta = (book.chapter_count || 0) + ' 章 · ' + fmtCount(book.total_chars);
    }
    return { format, label, meta };
  }

  function visibleBooks() {
    let list = statusFilter.value === 'archived' ? archivedBooks : books;
    if (viewMode === 'reading') {
      list = list.filter((b) => readingState(b) === 'reading');
    }
    if (formatFilter.value !== 'all') list = list.filter((b) => b.format === formatFilter.value);
    if (!['all', 'archived'].includes(statusFilter.value)) list = list.filter((b) => readingState(b) === statusFilter.value);
    const q = filterText.trim().toLowerCase();
    if (q) list = list.filter((b) => String(b.title || '').toLowerCase().indexOf(q) >= 0);
    return list.slice().sort((a, b) => {
      const mode = sortSelect.value;
      const n = mode === 'title' ? String(a.title).localeCompare(String(b.title), 'zh-CN', { numeric: true })
        : mode === 'read' ? (b.last_read_at || 0) - (a.last_read_at || 0)
          : mode === 'progress' ? (b.has_progress ? b.percent : 0) - (a.has_progress ? a.percent : 0)
            : (b.created_at || 0) - (a.created_at || 0);
      return n || String(a.id).localeCompare(String(b.id));
    });
  }

  /* ---------------- 卡片 ---------------- */
  function createCard(book) {
    const card = el('li', 'book-card');
    card.dataset.id = book.id;
    const link = el('a', 'book-open');
    const cover = el('div', 'book-cover');
    const badge = el('span', 'fmt-badge');
    const initial = el('span', 'cover-initial');
    const stateBadge = el('span', 'cover-state');
    const progress = el('span', 'cover-progress');
    const fill = el('i');
    progress.appendChild(fill);
    cover.append(badge, initial, stateBadge, progress);
    const info = el('div', 'book-info');
    const title = el('div', 'book-title');
    const meta = el('div', 'book-meta');
    const position = el('div', 'book-position');
    info.append(title, meta, position);
    link.append(cover, info);
    const more = el('button', 'book-more', '⋯');
    more.type = 'button';
    more.addEventListener('click', () => {
      const current = card._book;
      openBookDetails(current, fmtInfo(current).meta, async () => {
        await refreshBooks({ force: true });
      });
    });
    card.append(link, more);
    card._refs = { link, cover, badge, initial, stateBadge, progress, fill, title, meta, position, more, href: '' };
    return card;
  }

  function updateCard(card, book) {
    const refs = card._refs;
    card._book = book;
    const info = fmtInfo(book);
    // 交给阅读器恢复段落/字符位置，避免只传章节导致从章首重读。
    const href = '/?book=' + encodeURIComponent(book.id);
    if (refs.href !== href) { refs.link.href = href; refs.href = href; }
    const titleText = book.title || '未命名';
    if (refs.title.textContent !== titleText) {
      refs.title.textContent = titleText;
      refs.title.title = titleText;
    }
    const coverCls = 'book-cover cover-' + info.format;
    if (refs.cover.className !== coverCls) refs.cover.className = coverCls;
    if (refs.badge.textContent !== info.label) refs.badge.textContent = info.label;
    const initial = titleText;
    if (refs.initial.textContent !== initial) refs.initial.textContent = initial;
    refs.cover.style.setProperty('--book-hue', coverHue(book));
    refs.more.setAttribute('aria-label', titleText + '，更多操作');
    if (refs.meta.textContent !== info.meta) refs.meta.textContent = info.meta;
    const pct = Math.max(0, Math.min(1, Number(book.percent) || 0));
    const status = readingState(book);
    const finished = status === 'finished';
    refs.progress.hidden = status === 'unread';
    const width = (finished ? 100 : pct * 100).toFixed(1) + '%';
    if (refs.fill.style.width !== width) refs.fill.style.width = width;
    const stateText = finished ? '已读完' : (status === 'reading' ? '继续' : '未读');
    if (refs.stateBadge.textContent !== stateText) refs.stateBadge.textContent = stateText;
    card.classList.toggle('is-reading', status === 'reading');
    card.classList.toggle('is-finished', finished);
    refs.position.textContent = book.archived_at ? '已移出 · 可恢复' : readingLabel(book);
    refs.link.setAttribute('aria-label', titleText + '，' + info.meta + '，' + readingLabel(book));
  }

  function updateCounts(visible) {
    const total = books.length + ' 本';
    for (const node of countEls) if (node && node.textContent !== total) node.textContent = total;
    const filtering = !!filterText.trim() || formatFilter.value !== 'all' || statusFilter.value !== 'all';
    // 内容区标题只在「继续阅读」或搜索过滤时出现；默认不重复静态计数
    const showHead = statusFilter.value === 'archived' || books.length > 0 && (viewMode === 'reading' || filtering);
    if (shelfHeadEl) shelfHeadEl.hidden = !showHead;
    if (appMain) appMain.classList.toggle('no-shelf-head', !showHead);
    if (shelfTitleEl) {
      shelfTitleEl.textContent = statusFilter.value === 'archived' ? '已移出的书籍' : viewMode === 'reading' ? '继续阅读' : (filtering ? '筛选结果' : '全部书目');
    }
    if (shelfCountEl) {
      shelfCountEl.hidden = !showHead;
      const meta = statusFilter.value === 'archived' ? visible.length + ' 本可恢复' : viewMode === 'reading'
        ? visible.length + ' 本阅读中'
        : (filtering ? visible.length + ' / ' + books.length + ' 本' : '');
      if (meta && shelfCountEl.textContent !== meta) shelfCountEl.textContent = meta;
    }
  }

  /* ---------------- 原地 diff 渲染 ---------------- */
  function renderShelf() {
    const visible = visibleBooks();
    updateCounts(visible);
    const hasBooks = books.length > 0;
    if (libView) libView.classList.toggle('is-lib-empty', !hasBooks);
    if (emptyTextEl) emptyTextEl.textContent = EMPTY_TEXT;
    const isArchive = statusFilter.value === 'archived';
    emptyEl.hidden = isArchive || hasBooks || !loadedOnce;
    noresultEl.hidden = !((hasBooks || isArchive) && visible.length === 0);
    noresultEl.textContent = isArchive ? '没有已移出的书籍' : '没有符合条件的书籍';
    libView.classList.toggle('has-books', hasBooks);
    libView.classList.toggle('shelf-compact', density === 'compact');
    clearSearch.hidden = !filterText;
    filterInput.parentElement.classList.toggle('has-query', !!filterText);
    $('#density-standard').setAttribute('aria-pressed', String(density === 'standard'));
    $('#density-compact').setAttribute('aria-pressed', String(density === 'compact'));

    const scrollTop = appMain ? appMain.scrollTop : 0;
    const existing = new Map();
    for (const node of Array.from(listEl.children)) {
      const id = node.dataset && node.dataset.id;
      if (id) existing.set(id, node);
    }
    const order = new Set(visible.map((b) => b.id));
    const appeared = [];
    const frag = document.createDocumentFragment();
    for (const book of visible) {
      let card = existing.get(book.id);
      if (!card) {
        card = createCard(book);
        existing.set(book.id, card);
        if (loadedOnce && listEl._prevIds && !listEl._prevIds.has(book.id)) appeared.push(card);
      }
      updateCard(card, book);
      frag.appendChild(card);
    }
    for (const [id, node] of existing) {
      if (!order.has(id)) node.remove();
    }
    listEl.appendChild(frag);
    listEl._prevIds = new Set(visible.map((b) => b.id));
    // 实时刷新保留阅读位置（顶部新增书时不跳走）
    if (scrollTop && appMain) {
      appMain.scrollTop = scrollTop;
      requestAnimationFrame(() => { if (!destroyed && appMain) appMain.scrollTop = scrollTop; });
    }

    // 新增书入场（最多 3 张 stagger 40ms）
    appeared.forEach((card, i) => {
      card.style.animationDelay = Math.min(i, 2) * 40 + 'ms';
      card.classList.remove('is-new');
      void card.offsetWidth;
      card.classList.add('is-new');
      card.addEventListener('animationend', () => {
        card.classList.remove('is-new');
        card.style.animationDelay = '';
      }, { once: true });
    });

    // 首次进入：header + 卡片 stagger 淡入（仅一次）
    if (loadedOnce && !firstEntryPlayed && visible.length) {
      firstEntryPlayed = true;
      if (headerEl) {
        headerEl.classList.add('is-first');
        headerEl.addEventListener('animationend', () => headerEl.classList.remove('is-first'), { once: true });
      }
      visible.forEach((book, i) => {
        const card = listEl.querySelector('.book-card[data-id="' + book.id + '"]');
        if (!card) return;
        card.style.animationDelay = Math.min(i, 10) * 40 + 'ms';
        card.classList.add('is-first');
        card.addEventListener('animationend', () => {
          card.classList.remove('is-first');
          card.style.animationDelay = '';
        }, { once: true });
      });
    }
  }

  function applyBooks(next, opts = {}) {
    const list = Array.isArray(next) ? next : [];
    const fp = fingerprintOf(list);
    if (fp === fingerprint && !opts.force) return;
    fingerprint = fp;
    books = list;
    renderShelf();
  }

  async function loadArchived() {
    const request = ++archivedRequest;
    try {
      const data = await apiGet('/api/books?archived=true');
      if (request !== archivedRequest || destroyed) return;
      archivedBooks = data.books || [];
      hideError();
      renderShelf();
    } catch (err) {
      if (request !== archivedRequest || destroyed) return;
      showError(err);
      toast('已移出列表加载失败：' + err.message);
    }
  }

  function showError(err) {
    if (!errorEl) return;
    errorEl.hidden = false;
    if (errorTextEl) errorTextEl.textContent = '书架加载失败：' + ((err && err.message) || '未知错误');
  }
  function hideError() { if (errorEl && !errorEl.hidden) errorEl.hidden = true; }

  async function refreshBooks(opts = {}) {
    if (destroyed) return;
    try {
      const data = await apiGet('/api/books');
      if (destroyed) return;
      hideError();
      loadedOnce = true;
      applyBooks((data && data.books) || [], { force: !!opts.force });
      if (statusFilter.value === 'archived') await loadArchived();
      if (pollFailures) pollFailures = 0;
      return true;
    } catch (err) {
      if (destroyed) return;
      showError(err);
      if (opts.manual) toast('书架刷新失败：' + (err.message || '未知错误'));
      if (opts.poll) {
        pollFailures += 1;
        if (pollFailures >= 2) setLiveState('offline');
      }
    } finally {
      if (skeletonEl) skeletonEl.hidden = true;
    }
  }

  /* ---------------- 5s 轮询兜底 ---------------- */
  function startPolling() {
    if (pollTimer || destroyed) return;
    setLiveState('polling');
    pollTimer = setInterval(pollOnce, POLL_MS);
    if (pollKick) clearTimeout(pollKick);
    pollKick = setTimeout(() => { pollKick = 0; if (!destroyed && pollTimer) pollOnce(); }, 300);
  }
  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = 0; }
    if (pollKick) { clearTimeout(pollKick); pollKick = 0; }
  }
  async function pollOnce() {
    if (destroyed || pollInFlight) return;
    pollInFlight = true;
    try {
      if (!await refreshBooks({ poll: true }) || destroyed) return;
      if (es && es.readyState === 1) setLiveState('live');
      else if (liveState === 'offline') setLiveState('polling');
    } finally {
      pollInFlight = false;
      if (!es) openEventSource();
    }
  }

  /* ---------------- EventSource（SSE） ---------------- */
  function scheduleRefresh(delay) {
    if (destroyed) return;
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => { refreshTimer = 0; refreshBooks(); }, delay || 120);
  }
  function closeEventSource() {
    if (!es) return;
    try { es.close(); } catch (e) { /* ignore */ }
    es = null;
  }
  function openEventSource() {
    if (destroyed || es) return;
    if (typeof EventSource === 'undefined') { startPolling(); return; }
    let source;
    try { source = new EventSource('/api/events'); }
    catch (e) { startPolling(); return; }
    es = source;
    if (!pollTimer) setLiveState('connecting');
    source.addEventListener('open', () => {
      if (destroyed) return;
      stopPolling();
      pollFailures = 0;
      setLiveState('live');
    });
    source.addEventListener('books_changed', () => {
      if (destroyed) return;
      if (liveState !== 'live') setLiveState('live');
      scheduleRefresh(100);
    });
    source.addEventListener('error', () => {
      if (destroyed || !es) return;
      if (source.readyState === 2) {
        closeEventSource();
        startPolling();
        if (pollFailures >= 2) setLiveState('offline');
      } else {
        startPolling();
        setLiveState(pollFailures >= 2 ? 'offline' : 'polling');
      }
    });
  }

  /* ---------------- 生命周期 ---------------- */
  function destroy() {
    if (destroyed) return;
    destroyed = true;
    settingsPanels?.close(false);
    resetDrag();
    closeEventSource();
    stopPolling();
    if (refreshTimer) { clearTimeout(refreshTimer); refreshTimer = 0; }
    pollInFlight = false;
  }
  window.addEventListener('pagehide', destroy);
  window.addEventListener('beforeunload', destroy);
  window.addEventListener('pageshow', (e) => {
    if (!e.persisted || !destroyed) return;
    destroyed = false;
    importing = false;
    setImporting(-1);
    setLiveState('connecting');
    refreshBooks({ force: true });
    openEventSource();
  });

  /* ---------------- 视图切换 / 过滤 ---------------- */
  function setViewMode(mode) {
    viewMode = mode;
    statusFilter.value = 'all';
    const reading = mode === 'reading';
    navLibrary.classList.toggle('is-active', !reading);
    navContinue.classList.toggle('is-active', reading);
    if (reading) navContinue.setAttribute('aria-current', 'page'); else navContinue.removeAttribute('aria-current');
    if (!reading) navLibrary.setAttribute('aria-current', 'page'); else navLibrary.removeAttribute('aria-current');
    if (appMain) appMain.scrollTop = 0;
    renderShelf();
  }

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
      // 去重/恢复沿用本机或服务端的阅读位置；只有新书从开头开始。
      if (!res.deduped) store.set({ book: res.book.id, chapter: 0, para: 0, offset: 0 });
      toast(res.restored ? '已恢复到书架' : res.deduped ? '这本书已在书架中' : '导入成功');
      setImporting(1, file.name);
      setTimeout(() => {
        // 返回书架时页面可能由 bfcache 恢复，不能残留“正在导入”锁。
        importing = false;
        setImporting(-1);
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

  function pickFile() { if (!importing) input.click(); }

  /* ---------------- 事件 ---------------- */
  input.addEventListener('change', () => {
    importFile(input.files && input.files[0]);
    input.value = '';
  });
  drop.addEventListener('click', pickFile);
  drop.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pickFile(); }
  });
  // 文件拖到侧栏、搜索框、封面或空白处都走同一个导入入口。
  let dragDepth = 0;
  function hasFiles(e) {
    const transfer = e.dataTransfer;
    return transfer && (Array.from(transfer.types || []).includes('Files')
      || Array.from(transfer.items || []).some((item) => item.kind === 'file')
      || transfer.files.length > 0);
  }
  function resetDrag() {
    dragDepth = 0;
    drop.classList.remove('drag');
    dropOverlay.hidden = true;
  }
  function acceptDrag(e) {
    if (destroyed || libView.hidden || !hasFiles(e)) return false;
    e.preventDefault();
    e.dataTransfer.dropEffect = importing ? 'none' : 'copy';
    drop.classList.toggle('drag', !importing);
    dropOverlay.hidden = importing;
    return true;
  }
  document.addEventListener('dragenter', (e) => {
    if (acceptDrag(e)) dragDepth += 1;
  });
  document.addEventListener('dragover', acceptDrag);
  document.addEventListener('dragleave', () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (!dragDepth) resetDrag();
  });
  document.addEventListener('drop', (e) => {
    const isFileDrop = !destroyed && !libView.hidden && hasFiles(e);
    resetDrag();
    if (!isFileDrop) return;
    // 阻止 WebView 将拖入文件当成页面打开。
    e.preventDefault();
    const file = e.dataTransfer.files[0];
    importFile(file);
  });
  document.addEventListener('dragend', resetDrag);
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') resetDrag(); });
  window.addEventListener('blur', resetDrag);
  $('#lib-refresh').addEventListener('click', () => refreshBooks({ manual: true, force: true }));
  if ($('#rail-import')) $('#rail-import').addEventListener('click', pickFile);
  $('#rail-settings').addEventListener('click', openSettings);
  if ($('#lib-empty-pick')) $('#lib-empty-pick').addEventListener('click', pickFile);
  if ($('#lib-error-retry')) $('#lib-error-retry').addEventListener('click', () => refreshBooks({ manual: true, force: true }));
  if (navLibrary) navLibrary.addEventListener('click', () => setViewMode('all'));
  if (navContinue) navContinue.addEventListener('click', () => setViewMode('reading'));
  for (const select of [formatFilter, statusFilter, sortSelect]) select.addEventListener('change', () => {
    if (select === statusFilter) {
      viewMode = 'all'; navContinue.classList.remove('is-active'); navContinue.removeAttribute('aria-current');
      navLibrary.classList.add('is-active'); navLibrary.setAttribute('aria-current', 'page');
      if (statusFilter.value === 'archived') loadArchived();
    }
    saveShelfPreferences({ sort: sortSelect.value, density });
    renderShelf();
  });
  for (const value of ['standard', 'compact']) $('#density-' + value).addEventListener('click', () => {
    density = value; saveShelfPreferences({ sort: sortSelect.value, density }); renderShelf();
  });
  clearSearch.addEventListener('click', () => { filterInput.value = ''; filterText = ''; renderShelf(); filterInput.focus(); });
  document.addEventListener('keydown', (e) => {
    if (libView.hidden || settingsPanels?.isOpen() || e.altKey || e.ctrlKey || e.metaKey || document.querySelector('dialog[open]')) return;
    if (e.target.closest('input, textarea, select, [contenteditable="true"]')) return;
    if (e.key === '/') { e.preventDefault(); filterInput.focus(); }
  });
  if (filterInput) {
    filterInput.addEventListener('input', () => {
      filterText = filterInput.value || '';
      if (appMain) appMain.scrollTop = 0;
      renderShelf();
    });
    filterInput.addEventListener('keydown', (e) => { if (e.key === 'Escape') { filterInput.value = ''; filterText = ''; renderShelf(); } });
  }

  /* ---------------- 启动 ---------------- */
  setLiveState('connecting');
  await refreshBooks({ force: true });
  loadedOnce = true;
  if (skeletonEl) skeletonEl.hidden = true;
  renderShelf();
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
      viewMode: () => viewMode,
      filterText: () => filterText,
    };
  }

  return { destroy };
}
