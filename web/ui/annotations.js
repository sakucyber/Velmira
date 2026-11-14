/* ============================================================
   ui/annotations.js —— 注释/高亮交互层（懒加载，不进首屏请求）
   ------------------------------------------------------------
   职责：
   1) 选中正文 → 浮动菜单：4 色高亮 / 写笔记 / 复制
   2) 点击 <mark class="ann"> → 小菜单：改色 / 改笔记 / 删除
   3) 笔记编辑弹层（自定义，不用 window.prompt）
   坐标：{chapter, para, start_offset, end_offset} 单段坐标；
   选区跨段时收缩到起点所在段（契约 7.5 / M1 计划第 4 节）。
   DOM 高亮渲染由 paging.js 负责，本模块只发 API 并通知 reader 重建。
   ============================================================ */
import { el, apiPost, apiPatch, apiDelete, toast, getAuthState } from '../util/core.js';

const COLORS = [
  { id: 'yellow', name: '黄色' },
  { id: 'green', name: '绿色' },
  { id: 'blue', name: '蓝色' },
  { id: 'pink', name: '粉色' },
];

export function createAnnotationUI({ layer, bookId, column, getAnnotations, getContext, canActivate = () => true, onCreated, onUpdated, onDeleted }) {
  let menu = null;        // 当前浮动菜单
  let menuKind = null;    // 'selection' | 'mark'
  let pending = null;     // { chapter, para, start, end, text } 或已有 annotation 对象
  let modal = null;
  let hideTimer = 0;
  let hoverTimer = 0;
  let activeMark = null;
  let pinned = false;

  function annPath(id) {
    return '/api/books/' + encodeURIComponent(bookId) + '/annotations' + (id ? '/' + encodeURIComponent(id) : '');
  }

  function hideMenu() {
    clearTimeout(hoverTimer);
    if (activeMark) {
      activeMark.removeAttribute('aria-expanded');
      activeMark.removeAttribute('aria-controls');
    }
    activeMark = null;
    pinned = false;
    if (!menu) return;
    menu.remove();
    menu = null;
    menuKind = null;
  }

  function removeModal() {
    if (modal) { modal.remove(); modal = null; }
  }

  function hideAll() { hideMenu(); removeModal(); }

  /* ---------------- 选区 → 单段坐标 ---------------- */
  function paraOf(node) {
    let n = node;
    while (n && n !== column) {
      if (n.nodeType === 1 && n.hasAttribute('data-i')) return n;
      n = n.parentNode;
    }
    return null;
  }

  function textOffset(para, node, offset) {
    try {
      const r = document.createRange();
      r.setStart(para, 0);
      r.setEnd(node, offset);
      return r.toString().length;
    } catch (e) { return 0; }
  }

  function locatorFromSelection() {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
    const range = sel.getRangeAt(0);
    const startPara = paraOf(range.startContainer);
    if (!startPara) return null;
    const paraText = startPara.textContent || '';
    let start = textOffset(startPara, range.startContainer, range.startOffset);
    let end;
    const endPara = paraOf(range.endContainer);
    if (endPara && endPara !== startPara) {
      // 跨段选择一律收缩到起点所在段
      end = paraText.length;
    } else {
      end = textOffset(startPara, range.endContainer, range.endOffset);
    }
    start = Math.max(0, Math.min(start, paraText.length));
    end = Math.max(start, Math.min(end, paraText.length));
    // 去掉首尾空白，避免高亮把空格包进来
    while (start < end && /\s/.test(paraText[start])) start += 1;
    while (end > start && /\s/.test(paraText[end - 1])) end -= 1;
    if (end <= start) return null;
    return {
      para: Number(startPara.getAttribute('data-i')),
      start, end,
      text: paraText.slice(start, end),
      rect: range.getBoundingClientRect(),
      paraNode: startPara,
    };
  }

  /* ---------------- 浮动菜单定位 ---------------- */
  function place(node, rect) {
    document.body.appendChild(node);
    const w = node.offsetWidth || 232;
    const h = node.offsetHeight || 42;
    let left = rect.left + rect.width / 2 - w / 2;
    left = Math.max(8, Math.min(left, window.innerWidth - w - 8));
    // Android WebView also shows its native selection toolbar above the
    // selected text. Keep our annotation/AI actions below the selection on
    // narrow touch screens so that toolbar does not cover 翻译 and AI解释.
    const touchLayout = window.innerWidth <= 640 || window.matchMedia?.('(pointer: coarse)').matches;
    let top = touchLayout ? rect.bottom + 8 : rect.top - h - 8;
    if (top < 8 || top + h > window.innerHeight - 8) {
      top = Math.min(window.innerHeight - h - 8, rect.top - h - 8);
    }
    top = Math.max(8, top);
    node.style.left = Math.round(left) + 'px';
    node.style.top = Math.round(top) + 'px';
  }

  function colorDots(active, onPick) {
    const frag = document.createDocumentFragment();
    for (const c of COLORS) {
      const b = el('button', 'ann-color' + (active === c.id ? ' active' : ''));
      b.type = 'button';
      b.title = c.name + '高亮';
      b.setAttribute('data-color', c.id);
      b.setAttribute('aria-label', c.name + '高亮');
      const dot = el('span', 'ann-dot ann-' + c.id);
      b.appendChild(dot);
      b.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); onPick(c.id); });
      frag.appendChild(b);
    }
    return frag;
  }

  function actionBtn(label, title, onClick, cls = 'ann-act') {
    const b = el('button', cls, label);
    b.type = 'button';
    b.title = title;
    b.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); onClick(); });
    return b;
  }

  /* ---------------- 1) 选区菜单 ---------------- */
  function showSelectionMenu(loc, chapter) {
    hideMenu();
    menuKind = 'selection';
    pending = { chapter, para: loc.para, start: loc.start, end: loc.end, text: loc.text };
    menu = el('div', 'ann-float ann-float-sel');
    menu.setAttribute('role', 'dialog');
    menu.setAttribute('aria-label', '注释操作');
    menu.appendChild(colorDots(null, (color) => createAnnotation(color, null)));
    menu.appendChild(el('span', 'ann-sep'));
    menu.appendChild(actionBtn('笔记', '写笔记（同时黄色高亮）', () => openNoteEditor(null)));
    menu.appendChild(actionBtn('复制', '复制选中文字', copyPending));
    menu.appendChild(el('span', 'ann-sep'));
    menu.appendChild(actionBtn('翻译', '翻译选中文字', () => runSelectionAI('translate'), 'ann-act ann-act-ai'));
    menu.appendChild(actionBtn('AI解释', '解释选中文字', () => runSelectionAI('explain'), 'ann-act ann-act-ai'));
    place(menu, loc.rect);
  }

  async function copyPending() {
    const p = pending;
    hideAll();
    clearSelection();
    if (!p) return;
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) await navigator.clipboard.writeText(p.text);
      else throw new Error('no clipboard');
      toast('已复制 ' + p.text.length + ' 字');
    } catch (e) {
      toast('复制失败（浏览器未授权剪贴板）');
    }
  }

  /* ---------------- 选中文本 AI：翻译 / 解释 ---------------- */
  function selectionApi(mode) {
    const prefix = getAuthState().standalone ? '/api/mobile-ai/' : '/api/ai/';
    return prefix + (mode === 'translate' ? 'translate' : 'explain');
  }

  function translationTarget(text) {
    // The settings page describes the automatic policy: Chinese → English,
    // other source languages → Simplified Chinese. Users can still choose a
    // dedicated provider in settings; this hint only supplies its target.
    const han = (String(text).match(/[\u3400-\u9fff]/g) || []).length;
    return han >= Math.max(1, String(text).length * 0.08) ? 'English' : '简体中文';
  }

  function removeAiModal() {
    if (modal && modal.classList.contains('selection-ai-layer')) { modal.remove(); modal = null; }
  }

  // Render the model's explanation as a small, dependency-free Markdown
  // subset. Build DOM nodes instead of assigning innerHTML so model output can
  // never inject scripts, event handlers or unsafe URLs into the reader.
  function appendMarkdownInline(parent, source) {
    const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|__[^_\n]+__|\*[^*\n]+\*|_[^_\n]+_|\[([^\]\n]+)\]\(([^)\n]+)\))/g;
    let cursor = 0, match;
    const addText = text => { if (text) parent.append(document.createTextNode(text)); };
    while ((match = pattern.exec(String(source))) !== null) {
      addText(String(source).slice(cursor, match.index));
      const token = match[0];
      if (token.startsWith('`')) {
        const code = el('code', null, token.slice(1, -1)); parent.append(code);
      } else if (token.startsWith('**') || token.startsWith('__')) {
        parent.append(el('strong', null, token.slice(2, -2)));
      } else if (token.startsWith('*') || token.startsWith('_')) {
        parent.append(el('em', null, token.slice(1, -1)));
      } else {
        let url;
        try { url = new URL(match[3], location.href); } catch { url = null; }
        if (url && ['http:', 'https:'].includes(url.protocol)) {
          const link = el('a', null, match[2]); link.href = url.href; link.target = '_blank'; link.rel = 'noopener noreferrer'; parent.append(link);
        } else addText(token);
      }
      cursor = match.index + token.length;
    }
    addText(String(source).slice(cursor));
  }

  function renderMarkdown(parent, source) {
    parent.replaceChildren();
    const lines = String(source || '').replace(/\r\n?/g, '\n').split('\n');
    let list = null, ordered = false, inCode = false, codeLines = [];
    const endList = () => { list = null; };
    const flushCode = () => { if (!codeLines.length) return; parent.append(el('pre', 'selection-ai-code', codeLines.join('\n'))); codeLines = []; };
    for (const line of lines) {
      if (/^\s*```/.test(line)) { if (inCode) { flushCode(); inCode = false; } else { endList(); inCode = true; } continue; }
      if (inCode) { codeLines.push(line); continue; }
      if (!line.trim()) { endList(); continue; }
      const heading = line.match(/^\s*(#{1,6})\s+(.+)$/);
      const bullet = line.match(/^\s*[-*+]\s+(.+)$/);
      const numbered = line.match(/^\s*\d+[.)]\s+(.+)$/);
      if (heading) { endList(); const node = el('h' + heading[1].length, 'selection-ai-markdown-heading'); appendMarkdownInline(node, heading[2]); parent.append(node); continue; }
      if (bullet || numbered) {
        const isOrdered = !!numbered;
        if (!list || ordered !== isOrdered) { endList(); ordered = isOrdered; list = el(isOrdered ? 'ol' : 'ul', 'selection-ai-markdown-list'); parent.append(list); }
        const item = el('li'); appendMarkdownInline(item, (bullet || numbered)[1]); list.append(item); continue;
      }
      endList();
      const quote = line.match(/^\s*>\s?(.*)$/);
      const block = el(quote ? 'blockquote' : 'p'); appendMarkdownInline(block, quote ? quote[1] : line.trim()); parent.append(block);
    }
    if (inCode) flushCode();
  }

  function showAiModal(mode, text, result, loading = false) {
    removeAiModal();
    const title = mode === 'translate' ? '翻译结果' : 'AI 解释';
    modal = el('div', 'note-layer selection-ai-layer');
    const box = el('div', 'note-dialog selection-ai-dialog'); box.setAttribute('role', 'dialog'); box.setAttribute('aria-label', title);
    box.append(el('div', 'note-title', title), el('blockquote', 'note-quote', text));
    const output = el('div', 'selection-ai-output'); output.setAttribute('aria-live', 'polite');
    if (loading) output.textContent = '正在请求 AI…'; else if (mode === 'explain') renderMarkdown(output, result); else output.textContent = result;
    box.append(output);
    const acts = el('div', 'note-actions');
    const close = el('button', 'btn', '关闭'); close.type = 'button'; close.addEventListener('click', removeAiModal); acts.append(close);
    if (!loading) {
      const copy = el('button', 'btn btn-primary', '复制结果'); copy.type = 'button'; copy.addEventListener('click', async () => {
        try { await navigator.clipboard.writeText(result); toast('已复制 AI 结果'); } catch { toast('复制失败'); }
      }); acts.append(copy);
    }
    box.append(acts); modal.append(box);
    modal.addEventListener('click', e => { if (e.target === modal) removeAiModal(); });
    document.body.append(modal);
    if (!loading) setTimeout(() => close.focus(), 30);
  }

  async function runSelectionAI(mode) {
    const p = pending;
    hideMenu(); clearSelection();
    if (!p || !p.text) return;
    showAiModal(mode, p.text, '', true);
    try {
      let data;
      const context = getContext ? getContext(p.chapter, p.para, p.text) : p.text;
      data = await apiPost(selectionApi(mode), {
        text: p.text,
        context,
        ...(mode === 'translate' ? { target_language: translationTarget(p.text) } : {}),
        ...(getAuthState().standalone ? { book_id: bookId } : {}),
      });
      const result = String(data?.text || data?.result || data?.translation || data?.explanation || '').trim();
      if (!result) throw new Error('模型没有返回结果');
      showAiModal(mode, p.text, result);
    } catch (err) {
      showAiModal(mode, p.text, '请求失败：' + (err.message || '请检查文本模型设置'));
    }
  }

  async function createAnnotation(color, note) {
    const p = pending;
    hideAll();
    clearSelection();
    if (!p) return;
    try {
      const data = await apiPost(annPath(), {
        chapter: p.chapter, para: p.para,
        start_offset: p.start, end_offset: p.end,
        text: p.text, color, note: note || null,
      });
      const ann = data && data.annotation;
      if (ann) {
        if (onCreated) onCreated(ann);
        toast(note ? '已保存笔记' : '已添加高亮');
      }
    } catch (err) {
      toast('注释保存失败：' + (err.message || '未知错误'));
    }
  }

  function clearSelection() {
    try {
      const sel = window.getSelection();
      if (sel) sel.removeAllRanges();
    } catch (e) { /* ignore */ }
  }

  /* ---------------- 2) 点击高亮菜单 ---------------- */
  function markRect(mark, point) {
    // 多列/跨行文字的总包围盒可能跨越屏外列；以实际指向的可见行定位。
    const viewport = column.parentElement.getBoundingClientRect();
    const rects = [...mark.getClientRects()].filter((r) =>
      r.right > viewport.left && r.left < viewport.right && r.bottom > viewport.top && r.top < viewport.bottom);
    return rects.find((r) => point && point.clientX >= r.left && point.clientX <= r.right
      && point.clientY >= r.top && point.clientY <= r.bottom) || rects[0] || mark.getBoundingClientRect();
  }

  function showMarkMenu(ann, markNode, point, pin = false) {
    clearTimeout(hoverTimer);
    if (menuKind === 'mark' && activeMark === markNode) {
      pinned = pinned || pin;
      return;
    }
    hideMenu();
    menuKind = 'mark';
    pinned = pin;
    activeMark = markNode;
    pending = ann;
    menu = el('div', 'ann-float ann-float-mark');
    menu.id = 'ann-popover';
    menu.setAttribute('role', 'dialog');
    menu.setAttribute('aria-label', ann.note ? '批注' : '高亮操作');
    markNode.setAttribute('aria-expanded', 'true');
    markNode.setAttribute('aria-controls', menu.id);
    if (ann.note && ann.note.trim()) {
      menu.appendChild(el('div', 'ann-note-heading', '批注'));
      menu.appendChild(el('div', 'ann-note-content', ann.note));
    }
    const toolbar = el('div', 'ann-toolbar');
    toolbar.appendChild(colorDots(ann.color, async (color) => {
      if (color === ann.color) { hideMenu(); return; }
      hideMenu();
      try {
        const data = await apiPatch(annPath(ann.id), { color });
        if (data && data.annotation && onUpdated) onUpdated(data.annotation);
        toast('已改色');
      } catch (err) { toast('改色失败：' + (err.message || '未知错误')); }
    }));
    toolbar.appendChild(el('span', 'ann-sep'));
    toolbar.appendChild(actionBtn(ann.note ? '编辑' : '笔记', '编辑批注', () => openNoteEditor(ann)));
    toolbar.appendChild(actionBtn('删除', '删除该高亮及批注', () => removeAnnotation(ann), 'ann-act ann-act-danger'));
    menu.appendChild(toolbar);
    menu.addEventListener('pointerenter', () => clearTimeout(hoverTimer));
    menu.addEventListener('pointerleave', scheduleHoverHide);
    menu.addEventListener('focusin', () => { pinned = true; clearTimeout(hoverTimer); });
    menu.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        markNode.focus({ preventScroll: true });
        hideMenu();
      }
    });
    place(menu, markRect(markNode, point));
  }

  function scheduleHoverHide() {
    clearTimeout(hoverTimer);
    if (!pinned) hoverTimer = setTimeout(hideMenu, 180);
  }

  function annotationFor(mark) {
    return ((getAnnotations && getAnnotations()) || []).find((a) => String(a.id) === mark.dataset.ann);
  }

  async function removeAnnotation(ann) {
    hideAll();
    try {
      await apiDelete(annPath(ann.id));
      if (onDeleted) onDeleted(ann);
      toast('已删除');
    } catch (err) {
      toast('删除失败：' + (err.message || '未知错误'));
    }
  }

  /* ---------------- 3) 笔记编辑弹层 ---------------- */
  function openNoteEditor(ann) {
    const p = ann || pending;
    if (!p) return;
    hideMenu();
    removeModal();
    modal = el('div', 'note-layer');
    const box = el('div', 'note-dialog');
    box.setAttribute('role', 'dialog');
    box.setAttribute('aria-label', ann ? '编辑笔记' : '写笔记');
    box.appendChild(el('div', 'note-title', ann ? '编辑笔记' : '写笔记'));
    box.appendChild(el('blockquote', 'note-quote', p.text || ''));
    const input = el('textarea', 'note-input');
    input.placeholder = '写下你的想法…（Ctrl+Enter 保存）';
    input.value = (ann && ann.note) || '';
    box.appendChild(input);
    const acts = el('div', 'note-actions');
    const cancel = el('button', 'btn', '取消');
    cancel.type = 'button';
    cancel.addEventListener('click', () => { removeModal(); });
    const save = el('button', 'btn btn-primary', '保存');
    save.type = 'button';
    save.addEventListener('click', () => saveNote(ann, input.value.trim(), p));
    acts.appendChild(cancel);
    acts.appendChild(save);
    box.appendChild(acts);
    modal.appendChild(box);
    modal.addEventListener('click', (e) => { if (e.target === modal) removeModal(); });
    const onKey = (e) => {
      if (e.key === 'Escape') { e.preventDefault(); removeModal(); }
      else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); saveNote(ann, input.value.trim(), p); }
    };
    input.addEventListener('keydown', onKey);
    document.body.appendChild(modal);
    setTimeout(() => input.focus(), 30);
  }

  async function saveNote(ann, note, p) {
    removeModal();
    if (!p) return;
    try {
      let result;
      if (ann) {
        const data = await apiPatch(annPath(ann.id), { note: note || null });
        result = data && data.annotation;
        if (result && onUpdated) onUpdated(result);
        toast(note ? '笔记已更新' : '已清空笔记');
      } else {
        const data = await apiPost(annPath(), {
          chapter: p.chapter, para: p.para,
          start_offset: p.start, end_offset: p.end,
          text: p.text, color: p.color || 'yellow', note: note || null,
        });
        result = data && data.annotation;
        if (result && onCreated) onCreated(result);
        toast('已保存笔记');
      }
    } catch (err) {
      toast('笔记保存失败：' + (err.message || '未知错误'));
    }
  }

  /* ---------------- 事件绑定 ---------------- */
  function bind() {
    // 选区变化：有有效选区则弹菜单，无选区则收起（延迟避免与拖拽/点击冲突）
    document.addEventListener('selectionchange', () => {
      clearTimeout(hideTimer);
      hideTimer = setTimeout(() => {
        if (modal) return;
        const loc = locatorFromSelection();
        if (!loc) {
          if (menuKind === 'selection') hideMenu();
          return;
        }
        const sel = window.getSelection();
        if (sel && sel.rangeCount) {
          const range = sel.getRangeAt(0);
          loc.rect = range.getBoundingClientRect();
        }
        if (menuKind === 'selection' && menu) {
          // 同一菜单内继续拖动选区：更新坐标与位置，不重建
          pending = { chapter: currentChapter(), para: loc.para, start: loc.start, end: loc.end, text: loc.text };
          place(menu, loc.rect);
          return;
        }
        showSelectionMenu(loc, currentChapter());
      }, 140);
    });

    // 点击高亮：避免与翻页点击区冲突（mark 在正文内，优先处理）
    column.addEventListener('click', (e) => {
      const mark = e.target.closest ? e.target.closest('mark.ann') : null;
      if (!mark) return;
      const sel = window.getSelection();
      if (sel && String(sel).length > 0) return;
      e.stopPropagation();
      if (!canActivate() || modal) return;
      const ann = annotationFor(mark);
      if (!ann) return;
      showMarkMenu(ann, mark, e, true);
    }, true);

    column.addEventListener('pointerover', (e) => {
      if (e.pointerType !== 'mouse' || modal || !canActivate()) return;
      const mark = e.target.closest && e.target.closest('mark.ann-note');
      if (!mark || mark.contains(e.relatedTarget)) return;
      const sel = window.getSelection();
      if (sel && !sel.isCollapsed) return;
      // 已固定的批注保留到用户点击另一条或点空白处。
      if (pinned || menuKind === 'selection') return;
      const ann = annotationFor(mark);
      if (ann) showMarkMenu(ann, mark, e);
    });
    column.addEventListener('pointerout', (e) => {
      if (activeMark && activeMark.contains(e.target) && !activeMark.contains(e.relatedTarget)) scheduleHoverHide();
    });
    column.addEventListener('keydown', (e) => {
      const mark = e.target.closest && e.target.closest('mark.ann');
      if (!mark || (e.key !== 'Enter' && e.key !== ' ')) return;
      e.preventDefault();
      e.stopPropagation();
      const ann = annotationFor(mark);
      if (ann) showMarkMenu(ann, mark, null, true);
    });

    // 点空白处 / 滚动 / resize 收起菜单
    document.addEventListener('pointerdown', (e) => {
      if (menu && menu.contains(e.target)) return;
      if (e.target.closest && e.target.closest('mark.ann')) return;
      hideMenu();
    }, true);
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') { hideMenu(); removeModal(); }
    });
    window.addEventListener('resize', hideMenu, { passive: true });
    document.addEventListener('scroll', (e) => {
      if (menu && menu.contains(e.target)) return;
      hideMenu();
    }, { capture: true, passive: true });
    document.addEventListener('focusin', (e) => {
      if (menu && !menu.contains(e.target) && e.target !== activeMark) hideMenu();
    });
  }

  let chapterGetter = () => 0;
  function currentChapter() { return chapterGetter(); }
  function setChapterGetter(fn) { chapterGetter = fn; }

  return { bind, hideAll, hideMenu, setChapterGetter, openNoteEditor };
}
