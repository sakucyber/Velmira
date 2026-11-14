/* ============================================================
   ui/reader.js —— 文本阅读器视图
   职责：章节加载（abort + 销毁旧 DOM）、分页翻页、进度保存与恢复、
   目录/搜索/片段/主题/字号、书签、注释高亮、触摸滑动、
   键盘与点击区、URL 状态同步、debug 面板、CBZ 漫画委托入口。
   ============================================================ */
import {
  $, el, store, toast, debounce, clamp, intOrNull, fmtPercent,
  THEMES, FONT_SIZE, applyTheme, applyFontSize, normalizeFontSize,
  apiGet, apiPut, apiPost, apiDelete, apiPatch, getAuthState,
} from '../util/core.js';

export async function mountReader({ bookId, params, debug }) {
  const standalone = !!getAuthState().standalone;
  const dom = {
    title: $('#book-title'),
    chapterLabel: $('#chapter-label'),
    viewport: $('#reader-viewport'),
    column: $('#reader-column'),
    pageLabel: $('#page-label'),
    percentLabel: $('#percent-label'),
    finishChip: $('#finished-chip'),
    pagePrevBtn: $('#btn-page-prev'),
    pageNextBtn: $('#btn-page-next'),
    progressFill: $('#progress-fill'),
    progressTrack: $('#progress-track'),
    segmentsBtn: $('#btn-segments'),
    aiBtn: $('#btn-ai'),
    settingsBtn: $('#btn-settings'),
    bookmarkBtn: $('#btn-bookmark'),
    main: $('#reader-main'),
    layer: $('#drawer-layer'),
    debug: $('#debug-badge'),
  };

  const settings = store.get();
  let fontSize = applyFontSize(settings.fontSize);
  let theme = THEMES.some(t => t.id === settings.theme) ? settings.theme : 'day';
  applyTheme(theme);

  const state = {
    bookId,
    book: null,
    toc: null,
    chapterCount: 1,
    chapter: -1,
    para: 0,
    offset: 0,
    page: 0,
    pages: 1,
    chunk: 0,
    chunks: 1,
    vertical: false,
    percent: 0,
    segments: null,
    segmentsAvailable: true,
    bookmarks: [],
    bookmarksLoaded: false,
    annotations: [],
  };
  if (standalone) {
    state.segmentsAvailable = false;
    dom.segmentsBtn.hidden = true;
    dom.aiBtn.hidden = false;
    dom.aiBtn.title = '手机 AI：文本分析与生图';
  }

  // 分页内核按需动态加载：CBZ 漫画分支不需要 paging.js（省 1 个首屏请求）
  let pager = null;
  let rightComics = null, rightComicsPromise, autoStatus = null;
  let readingSync = null, readingSyncPromise = null;
  let readingHighlightNode = null;

  let panels = null;
  let chapterAc = null;
  let segmentsAc = null;
  let loadToken = 0;
  let tocPromise = null;
  let debugTimer = 0;
  let baselineHeap = 0;
  let chapterSwitchLog = [];
  let bookmarksPromise = null;
  let annUI = null;
  let swipeStart = null;
  let suppressClickUntil = 0;

  /* 章节编号判定 / 读完态状态（必须在首章加载前初始化） */
  const CHAPTER_MARK = /^(第\s*[0-9零〇一二三四五六七八九十百千万两]+\s*[章节回卷]|Chapter\s+\d+)/i;
  let finishSent = false;
  let atBookEnd = false;

  // 进度保存：翻页/切章后 1.2s 防抖写回服务端（失败静默）
  const scheduleSave = debounce(async () => {
    if (state.chapter < 0) return;
    try {
      const res = await apiPut('/api/books/' + encodeURIComponent(bookId) + '/progress', {
        chapter: state.chapter,
        para: Math.max(0, state.para),
        offset: state.offset,
      });
      if (res && res.progress) {
        state.percent = Number(res.progress.percent) || 0;
        updateProgressUI();
      }
    } catch (err) { /* 进度保存失败静默，不影响阅读 */ }
  }, 1200);

  /* ---------------- 加载书目 ---------------- */
  let bookData;
  try {
    bookData = await apiGet('/api/books/' + encodeURIComponent(bookId));
  } catch (err) {
    showFatal(err && err.status === 404 ? '这本书不存在或已被删除' : '书目加载失败，请返回书架重试');
    return;
  }
  state.book = bookData.book;
  const comicLink = state.book.reading_links?.comic_book_id ? el('a', 'btn btn-tool', '漫画') : null;
  if (comicLink) { comicLink.title = '切换到已导出的漫画'; dom.aiBtn.parentElement.appendChild(comicLink); }
  state.chapterCount = Math.max(1, state.book.chapter_count || 1);
  document.title = state.book.title + ' · Velmira';   // 契约 7.1：<书名> · Velmira
  dom.title.textContent = state.book.title;
  dom.title.title = state.book.title;

  /* ---------------- CBZ 漫画委托（契约 7.6） ---------------- */
  if ((state.book.format || 'txt') === 'cbz') {
    const ok = await mountComicDelegation(state.book);
    if (ok) return; // comic.js 已接管 #reader-main
    return;         // 兜底提示已渲染，不再进入文本阅读流程
  }

  /* 文本阅读：按需加载分页内核（动态 import，漫画分支不加载） */
  const { Pager } = await import('./paging.js');
  pager = new Pager(dom.viewport, dom.column, { onChange: onPagerChange });

  const qCh = intOrNull(params.get('ch'));
  const qPara = intOrNull(params.get('para'));
  const qP = intOrNull(params.get('p'));
  const qOffset = intOrNull(params.get('offset'));
  let progressFetched = false; // init 阶段是否已取过 /progress，避免首屏重复请求
  let target;
  if (qCh !== null) {
    target = {
      chapter: clamp(qCh, 0, state.chapterCount - 1),
      para: qPara !== null ? qPara : 0,
      offset: qOffset !== null ? Math.max(0, qOffset) : 0,
    };
  } else if (settings.book === bookId && Number.isInteger(settings.chapter)) {
    target = {
      chapter: clamp(settings.chapter, 0, state.chapterCount - 1),
      para: Math.max(0, settings.para | 0),
      offset: Math.max(0, settings.offset | 0),
    };
  } else {
    try {
      const pr = (await apiGet('/api/books/' + encodeURIComponent(bookId) + '/progress')).progress;
      progressFetched = true;
      target = {
        chapter: clamp(pr.chapter || 0, 0, state.chapterCount - 1),
        para: Math.max(0, pr.para | 0),
        offset: Math.max(0, pr.offset | 0),
      };
      state.percent = Number(pr.percent) || 0;
      updateProgressUI();
    } catch (err) {
      target = { chapter: 0, para: 0, offset: 0 };
    }
  }

  await loadChapter(target.chapter, target);
  // 深链只带 p（页码）、缺 para 时，按页码定位：p 是页码不是段号
  if (qCh !== null && qPara === null && qP !== null) pager.goToPage(qP);
  if (state.chapter < 0) return; // 首章失败时保留错误提示

  /* 首章就绪后补齐服务端进度百分比（仅 init 没取过 progress 时，避免重复请求） */
  if (qCh === null && !progressFetched) {
    apiGet('/api/books/' + encodeURIComponent(bookId) + '/progress')
      .then((data) => {
        if (data && data.progress && !state.percent) {
          state.percent = Number(data.progress.percent) || 0;
          updateProgressUI();
        }
      })
      .catch(() => {});
  }

  bindEvents();
  // Load the synchronized novel/comic view after the first chapter is ready.
  // A missing map is a normal state (no generated panels yet), so it must not
  // block or alter the ordinary text reader.
  startReadingSync();
  // Start after the reading viewport is ready; controls also remain available on phones.
  const autoToggle = el('button', 'btn btn-tool auto-manga-toggle', '自动漫画');
  autoToggle.type = 'button'; autoToggle.setAttribute('role', 'switch');
  autoToggle.setAttribute('aria-label', '自动生成连贯漫画'); autoToggle.setAttribute('aria-checked', 'false'); autoToggle.disabled = true;
  const autoView = el('button', 'btn btn-tool auto-manga-view', '漫画进度'); autoView.type = 'button';
  dom.settingsBtn.parentElement.prepend(autoToggle, autoView);
  if (standalone) { autoToggle.hidden = true; autoView.hidden = true; }
  let autoBusy = false, autoTimer;
  const autoAc = new AbortController();
  function showAuto(value) {
    autoStatus = value;
    autoToggle.setAttribute('aria-checked', String(value.enabled));
    autoToggle.classList.toggle('active', value.enabled);
    autoToggle.textContent = value.enabled ? '自动漫画 · 开' : '自动漫画';
    autoToggle.disabled = autoBusy;
    autoToggle.title = value.message;
    autoView.textContent = value.pages ? '漫画 · ' + value.pages + ' 页' : value.status === 'error' ? '漫画 · 已中断' : '漫画进度';
    if (readingSync) { readingSync.refresh?.(); return; }
    if (rightComics) rightComics.update(value, state.chapter);
    else if (value.pages && !rightComicsPromise) {
      rightComicsPromise = import('./reader-comics.js').then(({ mountReaderComics }) => {
        if (autoAc.signal.aborted) return;
        rightComics = mountReaderComics({ main: dom.main, bookId, signal: autoAc.signal, onLayout: () => pager.relocate().then(() => pager.reapplyAnnotations()) });
        rightComics.update(autoStatus, state.chapter);
      }).catch(() => { rightComicsPromise = null; });
    }
  }
  async function openAuto() {
    const p = await ensurePanels();
    p.openAutomatic({ chapter: () => state.chapter, onChange: showAuto });
  }
  autoView.addEventListener('click', openAuto);
  autoToggle.addEventListener('click', async () => {
    if (autoBusy || !autoStatus) return;
    autoBusy = true; autoToggle.disabled = true;
    try {
      const result = await apiPatch('/api/books/' + encodeURIComponent(bookId) + '/manga/auto', { enabled: !autoStatus.enabled, chapter: state.chapter });
      showAuto(result.auto);
      if (result.auto.enabled) await openAuto();
    } catch (err) { toast(err.message, 6000); await openAuto(); }
    finally { autoBusy = false; autoToggle.disabled = false; }
  });
  async function refreshAuto() {
    try {
      const result = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/manga/auto', autoAc.signal);
      if (!autoAc.signal.aborted && !autoBusy) showAuto(result.auto);
    } catch { /* Reading remains usable when the status service is temporarily unavailable. */ }
    if (!autoAc.signal.aborted) autoTimer = setTimeout(refreshAuto, 5000);
  }
  if (!standalone) setTimeout(refreshAuto, 800);
  window.addEventListener('pagehide', () => { autoAc.abort(); clearTimeout(autoTimer); }, { once: true });
  updatePercentFromUrl();
  updateDebug();

  // debug：首章渲染完成瞬间的资源清单，用于验收「首屏请求 ≤ 9」（之后的动态 import 不计入）
  if (debug) {
    window.__firstScreen = performance.getEntriesByType('resource').map((e) => e.name.replace(location.origin, ''));
  }

  // 首屏之后才加载注释交互层与书签列表（不占首屏请求预算）
  setTimeout(() => { ensureAnnUI().catch(() => {}); }, 0);
  setTimeout(() => { loadBookmarks().catch(() => {}); }, 600);

  /* 章节标题：源标题自带章节标记时直接显示，避免「第 2 章 · 第18章」双编号 */
  function chapterText(title, idx) {
    const raw = String(title || '').trim();
    if (!raw) return '第 ' + (idx + 1) + ' 章';
    if (CHAPTER_MARK.test(raw)) return raw;   // 以源标题为准，不再叠加界面序号
    return '第 ' + (idx + 1) + ' 章 · ' + raw;
  }

  /* 读完态：到达最后一章最后一页/滚动到底时写入末段末尾坐标，服务端 percent=1.0 */
  function isAtBookEnd() {
    if (state.chapter < 0 || state.chapter !== state.chapterCount - 1) return false;
    if (state.vertical) {
      const vp = dom.viewport;
      return vp.scrollTop + vp.clientHeight >= vp.scrollHeight - 8;
    }
    return state.chunk >= state.chunks - 1 && state.page >= state.pages - 1;
  }

  function setFinishUI(on) {
    atBookEnd = !!on;
    if (dom.finishChip) dom.finishChip.hidden = !on;
  }

  function checkFinished() {
    if (state.chapter < 0) return;
    const end = isAtBookEnd();
    setFinishUI(end);
    if (!end) { finishSent = false; return; }
    if (finishSent) return;
    finishSent = true;
    const paras = (pager && pager.paragraphs) || [];
    const lastIdx = Math.max(0, paras.length - 1);
    const lastLen = Array.from(String(paras[lastIdx] || '')).length;
    // 定位到全书最后一段末尾，服务端 percent 即为 1.0
    state.para = lastIdx;
    state.offset = lastLen;
    state.percent = 1;
    updateProgressUI();
    persist();
    syncUrl();
    const url = '/api/books/' + encodeURIComponent(bookId) + '/progress';
    fetch(url, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ chapter: state.chapter, para: lastIdx, offset: lastLen }),
      cache: 'no-store',
      keepalive: true,
    }).then((res) => (res && res.ok ? res.json() : null))
      .then((data) => {
        const p = data && data.progress ? Number(data.progress.percent) : NaN;
        if (Number.isFinite(p) && p > 0.999) { state.percent = 1; updateProgressUI(); }
      })
      .catch(() => { /* 读完进度保存失败：UI 保持 100%，下次翻页会再保存 */ });
  }

  function updateArrowState() {
    if (!dom.pagePrevBtn || !dom.pageNextBtn) return;
    const start = state.chapter <= 0 && state.page <= 0 && state.chunk <= 0 && !state.vertical;
    const end = isAtBookEnd();
    dom.pagePrevBtn.classList.toggle('is-edge', start);
    dom.pageNextBtn.classList.toggle('is-edge', end);
    dom.pagePrevBtn.title = start ? '已经是第一页' : '上一页';
    dom.pageNextBtn.title = end ? '已经是最后一页' : '下一页';
    dom.pagePrevBtn.setAttribute('aria-disabled', start ? 'true' : 'false');
    dom.pageNextBtn.setAttribute('aria-disabled', end ? 'true' : 'false');
  }

  /* 章节切换：正文一次 240ms 淡入（prefers-reduced-motion 下由 CSS 直接到位） */
  function playChapterIn() {
    dom.column.classList.remove('is-chapter-in');
    void dom.column.offsetWidth;
    dom.column.classList.add('is-chapter-in');
    setTimeout(() => dom.column.classList.remove('is-chapter-in'), 320);
  }

  /* ================= 核心：章节加载 ================= */
  async function loadChapter(idx, target = {}, opts = {}) {
    if (idx < 0 || idx >= state.chapterCount) return;
    if (annUI) annUI.hideAll();
    const token = ++loadToken;
    if (chapterAc) chapterAc.abort();
    if (segmentsAc) { segmentsAc.abort(); segmentsAc = null; }
    chapterAc = new AbortController();
    const ac = chapterAc;

    // 先销毁旧章 DOM 与正文引用，再请求新章（任何时刻不保留两章正文）
    pager.clear();
    state.chapter = idx;
    state.segments = null;
    state.para = Number(target.para) || 0;
    state.offset = Math.max(0, Number(target.offset) || 0);
    state.annotations = [];          // 内存约束：只驻留当前章注释
    dom.chapterLabel.textContent = '第 ' + (idx + 1) + ' 章 · 加载中…';
    dom.pageLabel.textContent = '— / —';
    finishSent = false;
    setFinishUI(false);
    updateBookmarkBtn();

    // 注释与章节并行拉取（接口未就绪/失败 → 静默降级为空，不阻塞阅读）
    const annPromise = apiGet(
      '/api/books/' + encodeURIComponent(bookId) + '/annotations?chapter=' + idx + '&limit=200', ac.signal
    ).then((d) => (d && Array.isArray(d.annotations) ? d.annotations : [])).catch(() => []);

    let data;
    try {
      data = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/chapters/' + idx, ac.signal);
    } catch (err) {
      if (err && err.name === 'AbortError') return;
      if (token !== loadToken) return;
      dom.chapterLabel.textContent = '第 ' + (idx + 1) + ' 章 · 加载失败';
      const errNode = el('p', 'load-error', '此章节尚未保存到本机，请联网打开或先下载这本书');
      dom.column.replaceChildren(errNode);
      toast('此章节尚未离线保存');
      return;
    }
    if (token !== loadToken) return;

    const chapterHead = chapterText(data.title, idx);
    dom.chapterLabel.textContent = chapterHead;
    dom.chapterLabel.title = chapterHead;
    const paragraphs = data.paragraphs || [];
    const illustrations = Array.isArray(data.illustrations) ? data.illustrations : [];
    const paragraphFormats = Array.isArray(data.paragraph_formats) ? data.paragraph_formats : [];
    const chapterTitle = data.title || '';
    data = null; // 释放章节 JSON 外壳，正文数组交给 pager 持有

    await pager.load(idx, chapterTitle, paragraphs, target, illustrations, paragraphFormats);
    if (token !== loadToken) return;
    playChapterIn();

    // 高亮必须在章节 DOM 之后按注释列表整章重建
    const anns = await annPromise;
    if (token !== loadToken) return;
    state.annotations = Array.isArray(anns) ? anns : [];
    pager.setAnnotations(state.annotations);

    syncUrl();
    persist();
    scheduleSave();
    if (panels && panels.isSegmentsOpen()) refreshSegments();
    updatePercentUI();
    updateBookmarkBtn();
    if (debug) logDebug('chapter');
    readingSync?.refresh?.();
  }

  function goChapter(idx) {
    if (idx < 0 || idx >= state.chapterCount) return;
    loadChapter(idx, { para: 0 });
  }

  async function pageNext(animate = true) {
    const moved = await pager.next({ animate });
    if (!moved) {
      if (state.chapter < state.chapterCount - 1) await loadChapter(state.chapter + 1, { para: 0 });
      else toast('已经是最后一章了');
    }
  }

  async function pagePrev(animate = true) {
    if (state.chapter <= 0 && pager.page <= 0 && pager.ci <= 0) {
      // 已在全书开头
      const moved = await pager.prev({ animate });
      if (!moved) toast('已经是第一页了');
      return;
    }
    const moved = await pager.prev({ animate });
    if (!moved && state.chapter > 0) await loadChapter(state.chapter - 1, { landAt: 'end' });
  }

  /* ================= 分页回调 / 进度 / URL ================= */
  function onPagerChange(info) {
    if (annUI) annUI.hideMenu();
    state.para = info.para;
    state.offset = Math.max(0, Number(info.offset) || 0);
    state.page = info.page;
    state.pages = info.pages;
    state.chunk = info.chunk;
    state.chunks = info.chunks;
    if (info.vertical !== state.vertical) {
      state.vertical = info.vertical;
      if (info.vertical) toast('当前浏览器不支持多列分页，已切换为滚动阅读');
    }
    dom.pageLabel.textContent = state.vertical
      ? '滚动阅读'
      : (state.page + 1) + ' / ' + state.pages + ' 页';
    syncUrl();
    persist();
    updateBookmarkBtn();
    scheduleSave();
    checkFinished();
    updateArrowState();
    updateDebug();
    if (readingSync) readingSync.update(info);
  }

  function startReadingSync() {
    if (readingSync || readingSyncPromise || standalone) return;
    readingSyncPromise = import('./reading-sync.js').then(({ mountReadingSync }) => mountReadingSync({
      bookId,
      main: dom.main,
      foot: document.querySelector('.reader-foot'),
      viewport: dom.viewport,
      pager,
      getState: () => state,
      getParagraphs: () => (pager && Array.isArray(pager.paragraphs) ? pager.paragraphs : []),
      navigate: jumpTo,
      onTextProgress: ({ chapter, para, offset }) => {
        if (chapter !== state.chapter || !pager) return;
        return pager.gotoPara(para, offset).then(() => {
          state.para = para; state.offset = offset;
          persist(); syncUrl(); scheduleSave();
          readingHighlightNode?.classList.remove('reading-sync-spoken');
          const node = dom.column.querySelector('[data-i="' + para + '"]');
          readingHighlightNode = node;
          node?.classList.add('reading-sync-spoken');
          try {
            if (!node || typeof Highlight === 'undefined' || !CSS.highlights) return;
            const points = Array.from(node.textContent || '');
            const start = points.slice(0, Math.max(0, Number(offset) || 0)).join('').length;
            const end = points.slice(0, Math.max(0, Number(offset) || 0) + 1).join('').length;
            const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
            const range = document.createRange(); let text; let count = 0; let foundStart = false;
            while ((text = walker.nextNode())) {
              const length = text.nodeValue.length;
              if (!foundStart && start <= count + length) { range.setStart(text, Math.max(0, start - count)); foundStart = true; }
              if (foundStart && end <= count + length) { range.setEnd(text, Math.max(0, end - count)); break; }
              count += length;
            }
            if (foundStart) CSS.highlights.set('velmira-reading-cursor', new Highlight(range));
          } catch (_) { /* Paragraph highlighting is the compatible fallback. */ }
        }).catch(() => {});
      },
    })).then((value) => {
      readingSync = value || null;
      if (readingSync) {
        rightComics?.destroy?.(); rightComics = null;
        dom.main.querySelectorAll('.reader-comics:not(.reader-sync-comics)').forEach(node => node.remove());
        pager.relocate().then(() => pager.reapplyAnnotations());
      }
      return readingSync;
    })
      .catch(() => null)
      .finally(() => { readingSyncPromise = null; });
    return readingSyncPromise;
  }

  function syncUrl() {
    if (state.chapter < 0) return;
    const url = location.pathname + '?book=' + encodeURIComponent(bookId)
      + '&ch=' + state.chapter + '&p=' + state.page
      + '&para=' + Math.max(0, state.para)
      + '&offset=' + Math.max(0, state.offset);
    try { history.replaceState({ book: bookId, ch: state.chapter, p: state.page, para: state.para, offset: state.offset }, '', url); } catch (e) { /* ignore */ }
  }

  function persist() {
    store.set({
      book: bookId,
      chapter: Math.max(0, state.chapter),
      para: Math.max(0, state.para),
      offset: state.offset,
      theme,
      fontSize,
    });
  }

  function updateProgressUI() {
    if (rightComics && autoStatus) rightComics.update(autoStatus, state.chapter);
    if (comicLink) {
      const links = state.book.reading_links;
      // Match the latest source paragraph at or before the reading position.
      let page = 0, best = null;
      links.pages.forEach((p, idx) => {
        if (p.chapter > state.chapter || (p.chapter === state.chapter && p.para > state.para)) return;
        if (!best || p.chapter > best.chapter || (p.chapter === best.chapter && p.para > best.para)) { page = idx; best = p; }
      });
      comicLink.href = '/?book=' + encodeURIComponent(links.comic_book_id) + '&ch=' + page;
    }
    const pct = clamp(state.percent, 0, 1);
    dom.progressFill.style.width = (pct * 100).toFixed(2) + '%';
    dom.progressTrack.setAttribute('aria-valuenow', String(Math.round(pct * 100)));
    dom.percentLabel.textContent = fmtPercent(pct);
  }
  function updatePercentUI() { updateProgressUI(); }

  function updatePercentFromUrl() {
    // URL 只带 ch/p 时没有百分比信息，等首次保存回填
    updateProgressUI();
  }

  /* ================= 跳转 / 高亮 ================= */
  async function jumpTo({ chapter, para = 0, offset = 0, q = null }) {
    if (chapter !== state.chapter) {
      await loadChapter(chapter, { para, offset });
    } else {
      await pager.gotoPara(para, offset);
    }
    // A programmatic jump may point inside a displayed page.  Keep its exact
    // character anchor rather than persisting only that page's first line.
    state.para = para; state.offset = offset;
    persist(); syncUrl(); scheduleSave();
    flashPara(para, q);
  }

  async function jumpToSegment({ chapter, start, end }) {
    if (chapter !== state.chapter) {
      await loadChapter(chapter, { para: start });
    } else {
      await pager.gotoPara(start);
    }
    flashRange(start, end);
  }

  function flashPara(para, q) {
    const node = dom.column.querySelector('[data-i="' + para + '"]');
    if (!node) return;
    node.classList.add('flash');
    if (q) highlightQuery(node, q);
    setTimeout(() => node.classList.remove('flash'), 3000);
  }

  function flashRange(start, end) {
    const nodes = [];
    for (let i = start; i <= end; i++) {
      const node = dom.column.querySelector('[data-i="' + i + '"]');
      if (node) { node.classList.add('flash'); nodes.push(node); }
    }
    setTimeout(() => nodes.forEach(n => n.classList.remove('flash')), 3000);
  }

  /* 笔记抽屉点击：跳转后闪烁对应高亮 */
  function flashAnnotation(id) {
    setTimeout(() => {
      const mark = dom.column.querySelector('mark.ann[data-ann="' + String(id).replace(/"/g, '\\"') + '"]');
      if (!mark) return;
      mark.classList.add('ann-flash');
      setTimeout(() => mark.classList.remove('ann-flash'), 3200);
    }, 60);
  }

  /* 用 CSS Custom Highlight API 精确高亮命中词（不修改 DOM、不触发重排） */
  function highlightQuery(node, q) {
    try {
      if (typeof Highlight === 'undefined' || !CSS.highlights || !node.firstChild) return;
      const text = node.textContent || '';
      const idx = text.indexOf(q);
      if (idx < 0) return;
      const range = document.createRange();
      range.setStart(node.firstChild, idx);
      range.setEnd(node.firstChild, Math.min(text.length, idx + q.length));
      CSS.highlights.set('velmira-search-hit', new Highlight(range));
      setTimeout(() => { try { CSS.highlights.delete('velmira-search-hit'); } catch (e) { /* ignore */ } }, 3000);
    } catch (e) { /* 不支持则只保留段级高亮 */ }
  }

  /* ================= 本章候选片段 ================= */
  function refreshSegments() {
    if (!state.segmentsAvailable) return;
    if (segmentsAc) segmentsAc.abort();
    const ac = new AbortController();
    segmentsAc = ac;
    const chapter = state.chapter;
    return apiGet('/api/books/' + encodeURIComponent(bookId) + '/segments?chapter=' + chapter + '&selected=1&limit=20', ac.signal)
      .then((data) => {
        if (ac.signal.aborted || chapter !== state.chapter) return;
        if (!data || !Array.isArray(data.segments)) { markSegmentsUnavailable(); return; }
        state.segments = data.segments;
        if (panels && panels.isSegmentsOpen()) panels.updateSegments(state.segments, chapter);
      })
      .catch((err) => {
        if (err && err.name === 'AbortError') return;
        if (chapter !== state.chapter) return;
        markSegmentsUnavailable();
      });
  }

  function markSegmentsUnavailable() {
    state.segmentsAvailable = false;
    state.segments = null;
    dom.segmentsBtn.hidden = true;
    if (panels && panels.isSegmentsOpen()) panels.close();
  }

  /* ================= M2 AI scene / 人物卡 ================= */
  async function loadAIStudio() {
    const p = await ensurePanels();
    if (standalone) { p.openMobileAI({ chapter: state.chapter, onSettings: openSettings }); return; }
    const render = (data) => p.updateAI(data, runAIExtract, freezeCharacter);
    p.openAI({ scenes: [], characters: [] }, runAIExtract, freezeCharacter);
    try {
      const [sceneData, characterData] = await Promise.all([
        apiGet('/api/books/' + encodeURIComponent(bookId) + '/ai/scenes'),
        apiGet('/api/books/' + encodeURIComponent(bookId) + '/ai/characters'),
      ]);
      render({ scenes: (sceneData && sceneData.scenes) || [], characters: (characterData && characterData.characters) || [] });
    } catch (err) {
      toast('AI 数据加载失败');
    }
  }

  async function runAIExtract() {
    if (!state.segments) {
      try { await refreshSegments(); } catch (err) { /* handled by refreshSegments */ }
    }
    try {
      let ids = (state.segments || []).filter((seg) => seg.selected !== 0).map((seg) => seg.id);
      if (!ids.length) {
        const existing = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/segments?limit=1');
        if (existing && existing.segments && existing.segments.length) {
          toast('当前章节没有入选片段，请切换到有候选片段的章节');
          return;
        }
        toast('正在准备候选片段…');
        await apiPost('/api/books/' + encodeURIComponent(bookId) + '/segment', { per_chapter: 1 });
        state.segmentsAvailable = true;
        dom.segmentsBtn.hidden = false;
        await refreshSegments();
        ids = (state.segments || []).filter((seg) => seg.selected !== 0).map((seg) => seg.id);
      }
      if (!ids.length) { toast('当前章节太短，尚无可分析的候选片段'); return; }
      const result = await apiPost('/api/books/' + encodeURIComponent(bookId) + '/ai/extract', { segment_ids: ids });
      toast(result && result.needs_review ? 'AI 完成，部分片段待复核' : 'AI scene 抽取完成');
      const [sceneData, characterData] = await Promise.all([
        apiGet('/api/books/' + encodeURIComponent(bookId) + '/ai/scenes'),
        apiGet('/api/books/' + encodeURIComponent(bookId) + '/ai/characters'),
      ]);
      const p = await ensurePanels();
      p.updateAI({ scenes: sceneData.scenes || [], characters: characterData.characters || [] }, runAIExtract, freezeCharacter);
    } catch (err) { toast('AI 抽取失败：' + (err.message || '未知错误')); }
  }

  async function freezeCharacter(character) {
    if (!character || !character.id) return;
    try {
      await apiPost('/api/books/' + encodeURIComponent(bookId) + '/ai/characters/' + encodeURIComponent(character.id) + '/freeze', {});
      toast('人物身份已冻结');
      await loadAIStudio();
    } catch (err) { toast('冻结失败：' + (err.message || '未知错误')); }
  }

  /* ================= 书签（契约 4.9） ================= */
  function bookmarkFor(chapter, para) {
    return state.bookmarks.find((b) => b.chapter === chapter && b.para === para) || null;
  }

  function updateBookmarkBtn() {
    if (!dom.bookmarkBtn) return;
    const on = state.chapter >= 0 && !!bookmarkFor(state.chapter, state.para);
    // 星标图标由内联 SVG 构成：激活时填充，不用字符替换
    dom.bookmarkBtn.setAttribute('aria-pressed', on ? 'true' : 'false');
    dom.bookmarkBtn.title = on ? '当前段已加书签，点击取消' : '书签当前段 (B)';
    dom.bookmarkBtn.setAttribute('aria-label', on ? '当前段已加书签，点击取消' : '书签当前段');
    dom.bookmarkBtn.classList.toggle('active', on);
  }

  function loadBookmarks(force = false) {
    if (state.bookmarksLoaded && !force) return Promise.resolve(state.bookmarks);
    if (bookmarksPromise && !force) return bookmarksPromise;
    bookmarksPromise = apiGet('/api/books/' + encodeURIComponent(bookId) + '/bookmarks')
      .then((data) => {
        state.bookmarks = (data && Array.isArray(data.bookmarks)) ? data.bookmarks : [];
        state.bookmarksLoaded = true;
        updateBookmarkBtn();
        return state.bookmarks;
      })
      .catch((err) => {
        bookmarksPromise = null;
        if (force) throw err;
        return [];
      });
    return bookmarksPromise;
  }

  async function toggleBookmark() {
    if (state.chapter < 0) return;
    const chapter = state.chapter;
    const para = Math.max(0, state.para);
    const existing = bookmarkFor(chapter, para);
    if (existing) { await removeBookmark(existing); return; }
    try {
      const res = await apiPost('/api/books/' + encodeURIComponent(bookId) + '/bookmarks', {
        chapter, para, offset: state.offset,
      });
      const bm = res && res.bookmark;
      if (bm) {
        state.bookmarks = [bm].concat(state.bookmarks.filter((item) => item.id !== bm.id));
        updateBookmarkBtn();
        if (panels && panels.isOpen()) panels.updateBookmarks(state.bookmarks, { chapter: state.chapter, para: state.para });
        toast('已加书签');
      }
    } catch (err) {
      toast('书签保存失败：' + (err.message || '未知错误'));
    }
  }

  async function removeBookmark(bm) {
    try {
      await apiDelete('/api/books/' + encodeURIComponent(bookId) + '/bookmarks/' + encodeURIComponent(bm.id));
      state.bookmarks = state.bookmarks.filter((x) => x.id !== bm.id);
      updateBookmarkBtn();
      if (panels && panels.isOpen()) panels.updateBookmarks(state.bookmarks, { chapter: state.chapter, para: state.para });
      toast('已移除书签');
    } catch (err) {
      toast('书签移除失败：' + (err.message || '未知错误'));
    }
  }

  /* ================= 注释/高亮（契约 7.5） ================= */
  async function ensureAnnUI() {
    if (annUI) return annUI;
    const mod = await import('./annotations.js');
    annUI = mod.createAnnotationUI({
      layer: dom.layer,
      bookId,
      column: dom.column,
      getAnnotations: () => state.annotations,
      getContext: (_chapter, para, selected) => {
        const paragraphs = (pager && Array.isArray(pager.paragraphs)) ? pager.paragraphs : [];
        const index = Math.max(0, Number(para) || 0);
        // Keep the request bounded while giving explanation/translation a little
        // surrounding context. The selected text is always included verbatim.
        return paragraphs.slice(Math.max(0, index - 1), Math.min(paragraphs.length, index + 2)).join('\n').slice(0, 6000) || selected;
      },
      canActivate: () => Date.now() >= suppressClickUntil,
      onCreated: onAnnotationCreated,
      onUpdated: onAnnotationUpdated,
      onDeleted: onAnnotationDeleted,
    });
    annUI.setChapterGetter(() => state.chapter);
    annUI.bind();
    return annUI;
  }

  function onAnnotationCreated(ann) {
    if (!ann || ann.chapter !== state.chapter) return;
    state.annotations.push(ann);
    pager.setAnnotations(state.annotations);
  }

  function onAnnotationUpdated(ann) {
    if (!ann) return;
    const i = state.annotations.findIndex((a) => a.id === ann.id);
    if (i >= 0) state.annotations[i] = ann;
    pager.setAnnotations(state.annotations);
    if (panels && panels.isOpen()) panels.updateNote(ann);
  }

  function onAnnotationDeleted(ann) {
    if (!ann) return;
    state.annotations = state.annotations.filter((a) => a.id !== ann.id);
    pager.setAnnotations(state.annotations);
  }

  /* ================= 主题 / 字号 ================= */
  let fontResize = null;
  let pendingFontSize = null;
  function changeFontSize(next) {
    pendingFontSize = normalizeFontSize(next);
    if (!fontResize) {
      fontResize = (async () => {
        // 拖动时合并连续输入，串行重排，避免旧测量覆盖新的字号。
        while (pendingFontSize !== null) {
          fontSize = applyFontSize(pendingFontSize); pendingFontSize = null;
          persist();
          await pager.relocate();
        }
        pager.reapplyAnnotations();
      })().finally(() => { fontResize = null; });
    }
    return fontResize;
  }

  async function openSettings(section) {
    if (annUI) annUI.hideMenu();
    const p = await ensurePanels();
    p.openSettings({ themes: THEMES, theme, fontRange: FONT_SIZE, fontSize,
      onTheme: (next) => {
        theme = next; applyTheme(theme); persist(); pager.reapplyAnnotations();
      },
      onFont: changeFontSize,
    }, typeof section === 'string' ? section : 'reading');
  }

  /* ================= 面板 ================= */
  async function ensurePanels() {
    if (!panels) {
      const mod = await import('./panels.js');
      panels = mod.createPanels({
        layer: dom.layer,
        bookId,
        onSettings: openSettings,
        onJump: jumpTo,
        onSegmentJump: jumpToSegment,
        onChapterSelect: goChapter,
        onNotesJump: async (an) => {
          await jumpTo({ chapter: an.chapter, para: an.para });
          flashAnnotation(an.id);
        },
        onNotesChanged: (ev) => {
          if (ev && ev.type === 'delete') onAnnotationDeleted(ev.annotation);
        },
        onBookmarkJump: (bm) => jumpTo(bm),
        onBookmarkDelete: (bm) => removeBookmark(bm),
      });
    }
    return panels;
  }

  /* ================= 事件绑定 ================= */
  function bindEvents() {
    $('#btn-toc').addEventListener('click', async () => {
      try {
        const chapters = await ensureToc();
        const p = await ensurePanels();
        p.openToc(chapters, state.chapter);
      } catch (err) { toast('目录加载失败'); }
    });
    $('#btn-search').addEventListener('click', async () => {
      const p = await ensurePanels();
      p.openSearch('');
    });
    dom.segmentsBtn.addEventListener('click', async () => {
      if (!state.segmentsAvailable) return;
      const p = await ensurePanels();
      p.openSegments(state.segments, state.chapter);
      if (!state.segments) refreshSegments(); // 按需拉取，不在切章时浪费请求
    });
    if (dom.aiBtn) dom.aiBtn.addEventListener('click', loadAIStudio);
    dom.settingsBtn.addEventListener('click', openSettings);

    // 桌面端左右翻页按钮（阻止 click 冒泡到 1/3 热区，避免一次点击翻两页）
    if (dom.pagePrevBtn) dom.pagePrevBtn.addEventListener('click', (e) => { e.stopPropagation(); pagePrev(); });
    if (dom.pageNextBtn) dom.pageNextBtn.addEventListener('click', (e) => { e.stopPropagation(); pageNext(); });

    // 书签按钮：当前段 toggle（同一段不重复添加）；列表按钮打开抽屉
    dom.bookmarkBtn.addEventListener('click', () => { toggleBookmark(); });
    $('#btn-bookmarks').addEventListener('click', async () => {
      try {
        await loadBookmarks(true);
        const p = await ensurePanels();
        p.openBookmarks(state.bookmarks, { chapter: state.chapter, para: state.para });
      } catch (err) { toast('书签加载失败'); }
    });
    $('#btn-notes').addEventListener('click', async () => {
      const p = await ensurePanels();
      p.openNotes(state.chapter);
    });

    document.addEventListener('keydown', onKeydown);

    // 翻页：点击左右 1/3 区域（事件委托；有选区或刚滑过时不误触）
    dom.viewport.addEventListener('click', (e) => {
      if (e.target.closest && e.target.closest('mark.ann')) return;
      if (Date.now() < suppressClickUntil) return;
      const sel = window.getSelection();
      if (sel && String(sel).length > 0) return;
      const rect = dom.viewport.getBoundingClientRect();
      const ratio = (e.clientX - rect.left) / Math.max(1, rect.width);
      if (ratio < 1 / 3) pagePrev();
      else if (ratio > 2 / 3) pageNext();
    });

    bindSwipe();

    window.addEventListener('resize', debounce(() => {
      if (state.chapter >= 0) pager.relocate().then(() => pager.reapplyAnnotations());
    }, 180));

    window.addEventListener('pagehide', () => {
      scheduleSave.flush();
      readingSync?.destroy?.();
      try { CSS.highlights?.delete('velmira-reading-cursor'); } catch (_) {}
      if (chapterAc) chapterAc.abort();
      if (segmentsAc) segmentsAc.abort();
      if (annUI) annUI.hideAll();
    });
  }

  /* ================= 触摸/触控笔滑动翻页（阈值 40px，契约 7.6） ================= */
  function bindSwipe() {
    dom.viewport.addEventListener('pointerdown', (e) => {
      if (e.pointerType === 'mouse' || e.isPrimary === false) return;
      swipeStart = { x: e.clientX, y: e.clientY, t: Date.now() };
    });
    dom.viewport.addEventListener('pointerup', (e) => {
      if (!swipeStart || e.pointerType === 'mouse') { swipeStart = null; return; }
      const s = swipeStart;
      swipeStart = null;
      const sel = window.getSelection();
      if (sel && String(sel).length > 0) return; // 长按选择文字时不平移页面
      const dx = e.clientX - s.x;
      const dy = e.clientY - s.y;
      if (Math.abs(dx) < 40) return;                       // 阈值 40px
      if (Math.abs(dx) <= Math.abs(dy) * 1.2) return;      // 更偏纵向 → 不翻页
      if (Date.now() - s.t > 900) return;                  // 缓慢拖拽不翻页
      suppressClickUntil = Date.now() + 450;               // 抑制随后的 click 误翻页
      if (dx < 0) pageNext(false); else pagePrev(false);   // 滑动翻页不加过渡
    });
    dom.viewport.addEventListener('pointercancel', () => { swipeStart = null; });
  }

  function onKeydown(e) {
    if (panels && panels.isOpen()) return;
    const t = e.target;
    if (t?.closest?.('.reader-comics')) return;
    if (t && t.closest && t.closest('.ann-float') && e.key !== 'Escape') return;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
    if (e.key === 'Escape') {
      if (panels && panels.isOpen()) panels.close();
      if (annUI) annUI.hideAll();
      return;
    }
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    switch (e.key) {
      case 'ArrowLeft': case 'PageUp': e.preventDefault(); pagePrev(); break;
      case 'ArrowRight': case 'PageDown': e.preventDefault(); pageNext(); break;
      case ' ': e.preventDefault(); if (e.shiftKey) pagePrev(); else pageNext(); break;
      case 't': case 'T': e.preventDefault(); $('#btn-toc').click(); break;
      case 'f': case 'F': e.preventDefault(); $('#btn-search').click(); break;
      case 'b': case 'B': e.preventDefault(); toggleBookmark(); break;
      default: break;
    }
  }

  async function ensureToc() {
    if (state.toc) return state.toc;
    if (!tocPromise) {
      tocPromise = apiGet('/api/books/' + encodeURIComponent(bookId) + '/toc')
        .then((data) => {
          state.toc = (data && data.chapters) || [];
          if (state.toc.length) state.chapterCount = state.toc.length;
          return state.toc;
        })
        .catch((err) => { tocPromise = null; throw err; });
    }
    return tocPromise;
  }

  /* ================= 漫画委托 / 兜底 ================= */
  async function mountComicDelegation(book) {
    try {
      const mod = await import('./comic.js');
      if (!mod || typeof mod.mountComic !== 'function') throw new Error('comic.js 未导出 mountComic');
      await mod.mountComic({ bookId, params, book });
      return true;
    } catch (err) {
      console.error('[velmira] comic mount failed', err);
      showComicFallback(err);
      return false;
    }
  }

  function showComicFallback(err) {
    const box = el('div', 'fatal comic-fallback');
    box.appendChild(el('p', 'fatal-msg', '漫画模块未就绪'));
    box.appendChild(el('p', 'fatal-sub', '这本书是 CBZ 漫画，但漫画阅读模块加载失败。'));
    if (err && err.message) box.appendChild(el('p', 'fatal-detail', String(err.message)));
    const row = el('div', 'fatal-actions');
    const back = el('a', 'btn', '返回书架');
    back.href = '/';
    const retry = el('button', 'btn btn-primary', '重试');
    retry.type = 'button';
    retry.addEventListener('click', () => location.reload());
    row.appendChild(back);
    row.appendChild(retry);
    box.appendChild(row);
    dom.main.replaceChildren(box);
  }

  /* ================= 错误页 ================= */
  function showFatal(msg) {
    dom.viewport.hidden = true;
    const box = el('div', 'fatal');
    box.appendChild(el('p', 'fatal-msg', msg));
    const back = el('a', 'btn', '返回书架');
    back.href = '/';
    box.appendChild(back);
    dom.main.appendChild(box);
  }

  /* ================= debug（?debug=1） ================= */
  function heapBytes() {
    try { return performance.memory ? performance.memory.usedJSHeapSize : 0; } catch (e) { return 0; }
  }
  function domNodes() { return document.querySelectorAll('*').length; }
  function logDebug(tag) {
    if (!debug) return;
    const heap = heapBytes();
    if (!baselineHeap && heap) baselineHeap = heap;
    chapterSwitchLog.push({ tag, chapter: state.chapter, heap, nodes: domNodes() });
    if (chapterSwitchLog.length > 40) chapterSwitchLog.shift();
    console.log('[velmira-debug]', tag, 'ch=' + (state.chapter + 1), 'heap=' + (heap / 1048576).toFixed(2) + 'MB',
      'nodes=' + domNodes(), 'pages=' + state.pages, 'chunks=' + state.chunks);
  }
  function updateDebug() {
    if (!debug) return;
    const heap = heapBytes();
    const text = (heap / 1048576).toFixed(2) + 'MB · ' + domNodes() + ' DOM · ch' + (state.chapter + 1)
      + ' · p' + (state.page + 1) + '/' + state.pages + ' · ' + (state.chunks || 1) + '块'
      + ' · 注' + state.annotations.length + ' 签' + state.bookmarks.length;
    dom.debug.textContent = text;
    dom.debug.hidden = false;
  }
  if (debug) {
    dom.debug.hidden = false;
    debugTimer = setInterval(updateDebug, 1200);
    window.__velmira = {
      state, pager,
      jumpTo, loadChapter, goChapter,
      next: pageNext, prev: pagePrev,
      nodes: domNodes, heap: heapBytes,
      baselineHeap: () => baselineHeap,
      switches: () => chapterSwitchLog.slice(),
      relocate: () => pager.relocate(),
      annotations: () => state.annotations,
      bookmarks: () => state.bookmarks,
      toggleBookmark, loadBookmarks,
      ensureAnnUI, ensurePanels,
      firstScreen: () => window.__firstScreen || [],
    };
  }

  updateProgressUI();
  return { state, pager };
}
