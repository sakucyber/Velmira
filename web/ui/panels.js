/* ============================================================
   ui/panels.js —— 抽屉面板：目录（可过滤）/ 搜索 / 本章候选片段 /
   书签列表 / 笔记与高亮列表 / 主题菜单
   ------------------------------------------------------------
   按需懒加载（dynamic import），不影响首屏请求预算。
   搜索/片段/注释接口失败时静默降级，不阻塞阅读。
   ============================================================ */
import { el, clear, debounce, apiGet, apiDelete, fmtTime, normalizeFontSize, toast } from '../util/core.js';

export function createPanels({ layer, bookId, onJump, onSegmentJump, onChapterSelect, onNotesJump, onNotesChanged, onBookmarkJump, onBookmarkDelete }) {
  let shell = null;        // 当前抽屉
  let body = null;
  let kind = null;         // 'toc' | 'search' | 'segments' | 'bookmarks' | 'notes'
  let fetchAc = null;      // 搜索/注释请求
  let popover = null;
  let popoverOff = null;
  let returnFocus = null;
  let disposePanel = null;

  layer.addEventListener('click', (e) => { if (e.target === layer) close(); });

  function removePopover() {
    if (popover) { popover.remove(); popover = null; }
    if (popoverOff) { document.removeEventListener('mousedown', popoverOff, true); popoverOff = null; }
  }

  function close(restoreFocus = true) {
    if (disposePanel) { disposePanel(); disposePanel = null; }
    removePopover();
    if (fetchAc) { fetchAc.abort(); fetchAc = null; }
    notes = [];            // 抽屉关闭即释放全书注释列表（内存约束）
    notesOffset = 0;
    notesDone = false;
    if (!shell) return;
    const dying = shell;
    shell = null;
    body = null;
    kind = null;
    dying.classList.remove('open');
    layer.classList.remove('open');
    setTimeout(() => {
      if (dying.parentNode) dying.parentNode.removeChild(dying);
      if (!shell) layer.hidden = true;
      if (!shell && restoreFocus && returnFocus?.isConnected) returnFocus.focus();
    }, 240);
  }

  function isOpen() { return !!shell; }
  function isSegmentsOpen() { return kind === 'segments'; }

  function open(nextKind, title, build) {
    const trigger = shell ? returnFocus : document.activeElement;
    close(false);
    returnFocus = trigger;
    kind = nextKind;
    shell = el('aside', 'drawer drawer-' + nextKind);
    shell.setAttribute('role', 'dialog');
    shell.setAttribute('aria-label', title);
    shell.setAttribute('aria-modal', 'true');
    shell.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); return; }
      if (e.key !== 'Tab') return;
      const items = [...shell.querySelectorAll('button, input, select, textarea, a[href], [tabindex="0"]')].filter((n) => !n.disabled && n.getClientRects().length);
      const first = items[0], last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last?.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first?.focus(); }
    });
    const head = el('div', 'drawer-head');
    head.appendChild(el('h2', 'drawer-title', title));
    const x = el('button', 'btn btn-icon drawer-close', '✕');
    x.type = 'button';
    x.title = '关闭';
    x.setAttribute('aria-label', '关闭面板');
    x.addEventListener('click', close);
    head.appendChild(x);
    body = el('div', 'drawer-body');
    shell.appendChild(head);
    shell.appendChild(body);
    layer.appendChild(shell);
    layer.hidden = false;
    build(body);
    requestAnimationFrame(() => {
      if (!shell) return;
      layer.classList.add('open');
      shell.classList.add('open');
      x.focus();
    });
  }

  /* ---------------- 目录（+ M1 过滤框：234 项输入即过滤） ---------------- */
  function openToc(chapters, currentChapter) {
    open('toc', '目录', (area) => {
      const box = el('div', 'toc-filter');
      const input = el('input', 'toc-filter-input');
      input.type = 'search';
      input.placeholder = '过滤章节（标题或序号）';
      input.setAttribute('aria-label', '过滤目录');
      box.appendChild(input);
      const meta = el('div', 'toc-filter-meta');
      const list = el('div', 'toc-list');
      area.appendChild(box);
      area.appendChild(meta);
      area.appendChild(list);

      const nodes = [];
      const frag = document.createDocumentFragment();
      for (const ch of chapters) {
        const item = el('button', 'toc-item' + (ch.idx === currentChapter ? ' active' : ''));
        item.type = 'button';
        item.appendChild(el('span', 'toc-idx', ch.idx + 1));
        item.appendChild(el('span', 'toc-name', ch.title));
        item.appendChild(el('span', 'toc-chars', (ch.chars || 0) + '字'));
        item.addEventListener('click', () => { close(); onChapterSelect(ch.idx); });
        item._q = (String(ch.idx + 1) + ' ' + (ch.title || '')).toLowerCase();
        nodes.push(item);
        frag.appendChild(item);
      }
      list.appendChild(frag);

      function applyFilter() {
        const q = input.value.trim().toLowerCase();
        let shown = 0;
        for (const item of nodes) {
          const hit = !q || item._q.indexOf(q) >= 0;
          item.hidden = !hit;
          if (hit) shown += 1;
        }
        meta.textContent = q ? '匹配 ' + shown + ' / ' + nodes.length + ' 章' : '共 ' + nodes.length + ' 章';
      }
      const run = debounce(applyFilter, 40);
      input.addEventListener('input', run);
      applyFilter();
      setTimeout(() => {
        const active = list.querySelector('.toc-item.active');
        if (active) active.scrollIntoView({ block: 'center' });
      }, 40);
      input.focus();
    });
  }

  /* ---------------- 搜索（250ms debounce，切面板时 abort） ---------------- */
  function openSearch(prefill) {
    open('search', '搜索原文', (area) => {
      const box = el('div', 'search-box');
      const input = el('input', 'search-input');
      input.type = 'search';
      input.placeholder = '输入关键词，如「示例」';
      input.value = prefill || '';
      box.appendChild(input);
      const meta = el('div', 'search-meta');
      const results = el('div', 'search-results');
      area.appendChild(box);
      area.appendChild(meta);
      area.appendChild(results);

      const run = debounce(async (q) => {
        clear(results);
        if (!q) { meta.textContent = ''; return; }
        if (fetchAc) fetchAc.abort();
        const ac = new AbortController();
        fetchAc = ac;
        meta.textContent = '搜索中…';
        try {
          const data = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/search?q=' + encodeURIComponent(q) + '&limit=20', ac.signal);
          if (ac.signal.aborted) return;
          const hits = (data && data.hits) || [];
          meta.textContent = hits.length
            ? '命中 ' + hits.length + ' 段 · ' + (data.elapsed_ms || 0) + 'ms' + (data.index && data.index.degraded ? ' · 降级检索' : '')
            : '没有找到匹配段落';
          const frag = document.createDocumentFragment();
          for (const hit of hits) {
            const item = el('button', 'search-hit');
            item.type = 'button';
            item.appendChild(el('span', 'hit-meta', '第 ' + (hit.chapter + 1) + ' 章 · 第 ' + (hit.para + 1) + ' 段'));
            const snip = el('span', 'hit-snippet');
            // 只信任后端转义后带 <b> 的 snippet 字段
            snip.innerHTML = hit.snippet || '';
            item.appendChild(snip);
            item.addEventListener('click', () => {
              close();
              onJump({ chapter: hit.chapter, para: hit.para, q });
            });
            frag.appendChild(item);
          }
          results.appendChild(frag);
        } catch (err) {
          if (err && err.name === 'AbortError') return;
          // 搜索接口不可用 → 静默降级，不阻塞阅读
          meta.textContent = '搜索暂不可用';
        }
      }, 250);

      input.addEventListener('input', () => run(input.value.trim()));
      setTimeout(() => input.focus(), 60);
      if (prefill) run(prefill);
    });
  }

  /* ---------------- 本章候选片段（数据由 reader 拉取缓存） ---------------- */
  function renderSegments(area, segments, chapter) {
    clear(area);
    if (!segments) {
      area.appendChild(el('div', 'drawer-empty', '加载中…'));
      return;
    }
    if (!segments.length) {
      area.appendChild(el('div', 'drawer-empty', '第 ' + (chapter + 1) + ' 章暂无候选片段'));
      return;
    }
    area.appendChild(el('div', 'seg-note', '本章共 ' + segments.length + ' 个候选片段 · 点击跳到原文并高亮'));
    const frag = document.createDocumentFragment();
    for (const seg of segments) {
      const item = el('button', 'seg-item');
      item.type = 'button';
      const head = el('div', 'seg-head');
      head.appendChild(el('span', 'seg-score', '评分 ' + Number(seg.score || 0).toFixed(3)));
      head.appendChild(el('span', null, (seg.chars || 0) + ' 字'));
      head.appendChild(el('span', null, '第 ' + (seg.start_para + 1) + '–' + (seg.end_para + 1) + ' 段'));
      const text = el('div', 'seg-text', seg.head || '');
      item.appendChild(head);
      item.appendChild(text);
      item.addEventListener('click', () => {
        close();
        onSegmentJump({ chapter: seg.chapter, start: seg.start_para, end: seg.end_para });
      });
      frag.appendChild(item);
    }
    area.appendChild(frag);
  }

  function openSegments(segments, chapter) {
    open('segments', '本章候选片段', (area) => renderSegments(area, segments, chapter));
  }

  function updateSegments(segments, chapter) {
    if (!shell || kind !== 'segments') return;
    renderSegments(body, segments, chapter);
  }

  /* ---------------- M2 AI 分析与人物卡 ---------------- */
  function renderAI(area, data, onExtract, onFreeze) {
    clear(area);
    const scenes = (data && Array.isArray(data.scenes)) ? data.scenes : [];
    const characters = (data && Array.isArray(data.characters)) ? data.characters : [];
    const extract = el('button', 'btn ai-extract', '准备片段并分析本章');
    extract.type = 'button';
    extract.addEventListener('click', async () => {
      extract.disabled = true;
      extract.textContent = '分析中…';
      try { await onExtract(); } finally { extract.disabled = false; extract.textContent = '准备片段并分析本章'; }
    });
    area.appendChild(extract);
    const manga = el('button', 'btn ai-extract', '漫画生成');
    manga.type = 'button';
    manga.addEventListener('click', openManga);
    area.appendChild(manga);
    area.appendChild(el('div', 'seg-note', scenes.length ? ('已生成 ' + scenes.length + ' 个 scene') : '尚未生成 scene；默认使用离线 Mock provider'));
    for (const scene of scenes) {
      const card = el('article', 'ai-scene-card');
      card.appendChild(el('div', 'ai-scene-title', '第 ' + ((scene.chapter_id | 0) + 1) + ' 章 · ' + ((scene.plan && scene.plan.summary) || '未命名场景')));
      const plan = scene.plan || {};
      const meta = [plan.location, plan.time_of_day, plan.drawable == null ? '' : ('可绘制度 ' + Math.round(Number(plan.drawable) * 100) + '%')].filter(Boolean).join(' · ');
      if (meta) card.appendChild(el('div', 'ai-scene-meta', meta));
      if (plan.reason) card.appendChild(el('div', 'ai-scene-reason', plan.reason));
      area.appendChild(card);
    }
    area.appendChild(el('div', 'ai-section-title', '人物卡 · ' + characters.length));
    if (!characters.length) area.appendChild(el('div', 'drawer-empty', '抽取 scene 后会在这里出现人物卡'));
    for (const character of characters) {
      const card = el('article', 'ai-character-card');
      const head = el('div', 'ai-character-head');
      head.appendChild(el('strong', null, character.name || '未命名'));
      head.appendChild(el('span', 'ai-character-state', character.frozen ? '已冻结' : '草稿'));
      card.appendChild(head);
      const identity = character.identity_block || (character.card && Object.entries(character.card).filter(([key]) => key !== 'evidence' && key !== 'identity_history').map(([key, value]) => key + ': ' + value).join(' · '));
      if (identity) card.appendChild(el('div', 'ai-identity', identity));
      const actions = el('div', 'ai-character-actions');
      if (!character.frozen) {
        const freeze = el('button', 'btn btn-ghost', '冻结身份');
        freeze.type = 'button';
        freeze.addEventListener('click', () => onFreeze(character));
        actions.appendChild(freeze);
      }
      card.appendChild(actions);
      area.appendChild(card);
    }
  }

  function openAI(data, onExtract, onFreeze) {
    open('ai', 'AI 分析与人物卡', (area) => renderAI(area, data, onExtract, onFreeze));
  }

  function updateAI(data, onExtract, onFreeze) {
    if (!shell || kind !== 'ai') return;
    renderAI(body, data, onExtract, onFreeze);
  }

  function openManga() {
    open('manga', '漫画生成', (area) => {
      const ac = new AbortController();
      disposePanel = () => ac.abort();
      area.appendChild(el('p', 'seg-note', '正在加载…'));
      import('./manga.js').then(({ mountManga }) => {
        if (!ac.signal.aborted) mountManga({ area, bookId, signal: ac.signal });
      }).catch(() => { if (!ac.signal.aborted) area.textContent = '漫画面板加载失败，请重新打开'; });
    });
  }

  /* ---------------- 书签抽屉（数据由 reader 持有并拉取） ---------------- */
  function renderBookmarks(area, bookmarks, current) {
    clear(area);
    if (!bookmarks.length) {
      area.appendChild(el('div', 'drawer-empty', '还没有书签 · 点顶栏 ☆ 收藏当前段落'));
      return;
    }
    area.appendChild(el('div', 'seg-note', '共 ' + bookmarks.length + ' 条书签 · 点条目跳转'));
    const frag = document.createDocumentFragment();
    for (const bm of bookmarks) {
      const item = el('div', 'bm-item' + (current && bm.chapter === current.chapter && bm.para === current.para ? ' active' : ''));
      const jump = el('button', 'bm-jump');
      jump.type = 'button';
      const line = el('span', 'bm-loc', '第 ' + ((bm.chapter | 0) + 1) + ' 章 · 第 ' + ((bm.para | 0) + 1) + ' 段');
      jump.appendChild(line);
      if (bm.label) jump.appendChild(el('span', 'bm-label', bm.label));
      jump.appendChild(el('span', 'bm-time', fmtTime(bm.created_at)));
      jump.addEventListener('click', () => { close(); onBookmarkJump(bm); });
      const del = el('button', 'bm-del btn btn-ghost', '删除');
      del.type = 'button';
      del.title = '删除书签';
      del.addEventListener('click', () => onBookmarkDelete(bm));
      item.appendChild(jump);
      item.appendChild(del);
      frag.appendChild(item);
    }
    area.appendChild(frag);
  }

  function openBookmarks(bookmarks, current) {
    open('bookmarks', '书签', (area) => renderBookmarks(area, bookmarks, current));
  }

  function updateBookmarks(bookmarks, current) {
    if (!shell || kind !== 'bookmarks') return;
    renderBookmarks(body, bookmarks, current);
  }

  /* ---------------- 笔记与高亮抽屉（全书，按章分组，懒加载分页） ---------------- */
  const NOTES_PAGE = 100;
  let notes = [];
  let notesOffset = 0;
  let notesDone = false;
  let notesMore = null;
  let notesChapter = -1;

  function renderNotes(area, currentChapter) {
    clear(area);
    if (!notes.length && notesDone) {
      area.appendChild(el('div', 'drawer-empty', '还没有笔记或高亮 · 选中正文即可标注'));
      return;
    }
    area.appendChild(el('div', 'seg-note', '共 ' + notes.length + (notesDone ? '' : '+') + ' 条注释 · 点条目跳转并闪烁'));
    const frag = document.createDocumentFragment();
    let lastChapter = -1;
    for (const an of notes) {
      if (an.chapter !== lastChapter) {
        lastChapter = an.chapter;
        frag.appendChild(el('div', 'note-group' + (an.chapter === currentChapter ? ' active' : ''),
          '第 ' + (an.chapter + 1) + ' 章'));
      }
      const item = el('button', 'note-item ann-' + (an.color || 'yellow'));
      item.type = 'button';
      const head = el('div', 'note-head');
      head.appendChild(el('span', 'note-color', ''));
      head.appendChild(el('span', 'note-loc', '第 ' + (an.para + 1) + ' 段 · ' + fmtTime(an.updated_at || an.created_at)));
      const del = el('span', 'note-del');
      del.textContent = '删除';
      del.title = '删除该注释';
      del.addEventListener('click', (e) => { e.stopPropagation(); removeNote(an, area); });
      head.appendChild(del);
      item.appendChild(head);
      item.appendChild(el('div', 'note-text', an.text || ''));
      if (an.note) item.appendChild(el('div', 'note-body', '笔记：' + an.note));
      item.addEventListener('click', () => {
        close();
        if (onNotesJump) onNotesJump(an);
      });
      frag.appendChild(item);
    }
    area.appendChild(frag);
    notesMore = el('button', 'btn notes-more');
    notesMore.type = 'button';
    if (notesDone) {
      notesMore.textContent = '已加载全部';
      notesMore.disabled = true;
    } else {
      notesMore.textContent = '加载更多';
      notesMore.addEventListener('click', () => loadNotes(area));
    }
    area.appendChild(notesMore);
  }

  async function removeNote(an, area) {
    try {
      await apiDelete('/api/books/' + encodeURIComponent(bookId) + '/annotations/' + encodeURIComponent(an.id));
      notes = notes.filter((x) => x.id !== an.id);
      if (onNotesChanged) onNotesChanged({ type: 'delete', annotation: an });
      renderNotes(area, notesChapter);
    } catch (err) {
      if (notesMore) notesMore.textContent = '删除失败';
    }
  }

  async function loadNotes(area) {
    if (fetchAc) fetchAc.abort();
    const ac = new AbortController();
    fetchAc = ac;
    try {
      const data = await apiGet('/api/books/' + encodeURIComponent(bookId)
        + '/annotations?limit=' + NOTES_PAGE + '&offset=' + notesOffset, ac.signal);
      if (ac.signal.aborted) return;
      const list = (data && data.annotations) || [];
      notes = notes.concat(list);
      notesOffset += list.length;
      if (list.length < NOTES_PAGE) notesDone = true;
      renderNotes(area, notesChapter);
    } catch (err) {
      if (err && err.name === 'AbortError') return;
      if (!notes.length) {
        clear(area);
        area.appendChild(el('div', 'drawer-empty', '注释接口暂不可用（后端未就绪时会静默降级）'));
      }
    }
  }

  function openNotes(currentChapter) {
    notes = [];
    notesOffset = 0;
    notesDone = false;
    notesChapter = Number(currentChapter) || 0;
    open('notes', '笔记与高亮', (area) => loadNotes(area));
  }

  function updateNote(annotation) {
    if (!shell || kind !== 'notes') return;
    const i = notes.findIndex((x) => x.id === annotation.id);
    if (i >= 0) notes[i] = annotation;
    renderNotes(body, notesChapter);
  }

  function openSettings(options) {
    open('settings', '阅读设置', (area) => {
      area.append(el('h3', 'settings-heading', '阅读显示'));
      area.append(el('p', 'settings-help', '设置即时生效，并在本机记住。'));
      const title = el('div', 'settings-label', '主题');
      const themes = el('div', 'settings-themes');
      themes.setAttribute('role', 'group'); themes.setAttribute('aria-label', '阅读主题');
      for (const theme of options.themes) {
        const button = el('button', 'btn settings-theme', theme.name);
        button.type = 'button'; button.dataset.themeChoice = theme.id;
        button.setAttribute('aria-pressed', String(theme.id === options.theme));
        const swatch = el('i', 'settings-swatch'); swatch.style.background = theme.swatch;
        button.prepend(swatch);
        button.addEventListener('click', () => {
          options.onTheme(theme.id);
          for (const b of themes.children) b.setAttribute('aria-pressed', String(b === button));
        });
        themes.append(button);
      }
      area.append(title, themes);
      if (options.fontRange) {
        const group = el('div', 'settings-font');
        const head = el('div', 'settings-font-head');
        const valueLabel = el('label', 'settings-font-value');
        const number = el('input', 'shelf-input'); number.type = 'number'; number.inputMode = 'decimal'; number.setAttribute('aria-label', '阅读字号');
        const range = el('input', 'settings-font-range'); range.type = 'range'; range.setAttribute('aria-label', '调整阅读字号');
        const limits = options.fontRange;
        for (const input of [number, range]) { input.min = limits.min; input.max = limits.max; input.step = limits.step; input.value = options.fontSize; }
        valueLabel.append(number, el('span', null, 'px'));
        head.append(el('span', 'settings-label', '字号'), valueLabel);
        let current = options.fontSize;
        function update(value, syncNumber = true) {
          current = normalizeFontSize(value); range.value = current;
          range.setAttribute('aria-valuetext', current + ' 像素');
          if (syncNumber) number.value = current;
          options.onFont(current).catch(() => toast('字号调整失败，请重试'));
        }
        range.setAttribute('aria-valuetext', current + ' 像素');
        range.addEventListener('input', () => update(range.value));
        number.addEventListener('input', () => { if (number.value !== '' && number.validity.valid) update(number.value, false); });
        number.addEventListener('change', () => update(number.value === '' ? current : number.value));
        number.addEventListener('blur', () => { number.value = current; });
        number.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); update(number.value === '' ? current : number.value); } });
        const foot = el('div', 'settings-font-foot');
        const reset = el('button', 'btn', '恢复默认'); reset.type = 'button';
        reset.addEventListener('click', () => update(limits.default));
        foot.append(el('span', 'settings-help', limits.min + '–' + limits.max + ' px · 可拖动或直接输入'), reset);
        group.append(head, range, foot); area.append(group);
      }
      if (options.onFit) {
        const label = el('label', 'settings-field', '漫画显示');
        const select = el('select', 'shelf-input'); select.setAttribute('aria-label', '漫画显示方式');
        for (const [value, name] of [['page', '适页'], ['width', '适宽']]) { const item = el('option', null, name); item.value = value; select.append(item); }
        select.value = options.fit; select.addEventListener('change', () => options.onFit(select.value));
        label.append(select); area.append(label);
      }
      area.append(el('h3', 'settings-heading', '快捷键'));
      const shortcuts = options.comic
        ? [['← / Page Up', '上一页'], ['→ / Page Down / 空格', '下一页'], ['Esc', '关闭设置']]
        : [['← / Page Up', '上一页'], ['→ / Page Down / 空格', '下一页'], ['Shift + 空格', '上一页'], ['T', '打开目录'], ['F', '搜索正文'], ['B', '添加或取消当前段书签'], ['Esc', '关闭面板或批注弹层']];
      const list = el('dl', 'settings-shortcuts');
      for (const [keys, action] of shortcuts) { const term = el('dt'); term.append(el('kbd', null, keys)); list.append(term, el('dd', null, action)); }
      area.append(list, el('p', 'settings-help', '输入文字或打开设置时，翻页快捷键暂停。触屏可左右滑动翻页，点击批注文字可查看内容。'));
    });
  }

  /* ---------------- 主题菜单（小弹出层） ---------------- */
  function openTheme(anchor, themes, current, onChange) {
    removePopover();
    popover = el('div', 'popover');
    popover.setAttribute('role', 'menu');
    for (const t of themes) {
      const item = el('button', current === t.id ? 'active' : '');
      item.type = 'button';
      const swatch = el('span', 'swatch');
      swatch.style.background = t.swatch;
      item.appendChild(swatch);
      item.appendChild(el('span', null, t.name));
      item.addEventListener('click', () => { removePopover(); onChange(t.id); });
      popover.appendChild(item);
    }
    document.body.appendChild(popover);
    const r = anchor.getBoundingClientRect();
    const width = popover.offsetWidth || 132;
    let left = r.right - width;
    left = Math.max(8, Math.min(left, window.innerWidth - width - 8));
    popover.style.left = left + 'px';
    popover.style.top = Math.min(r.bottom + 6, window.innerHeight - popover.offsetHeight - 8) + 'px';
    popoverOff = (ev) => {
      if (popover && popover.contains(ev.target)) return;
      removePopover();
    };
    setTimeout(() => document.addEventListener('mousedown', popoverOff, true), 0);
  }

  return {
    openToc, openSearch, openSegments, updateSegments, openTheme, openSettings,
    openBookmarks, updateBookmarks, openNotes, updateNote, openAI, updateAI,
    close, isOpen, isSegmentsOpen,
  };
}
