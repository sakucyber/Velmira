/* ============================================================
   ui/paging.js —— 分页内核（CSS 多列 + translateX）
   ------------------------------------------------------------
   原理：viewport(overflow:hidden, 固定高) >
         column(column-width:100%, column-gap:0, column-fill:auto, height:100%)
         内容在视口高度处切成一列一列排到右侧，页数 = 总宽 / 视口宽。
   翻页只改 transform（不读 layout）；测量只在章/块加载、resize、字号变化时做。
   大章按段落切块（≤8000 字/块），一次只渲染一块，块边界翻页自动衔接。
   列布局失效（长文本仍只有 1 页）时切换为垂直滚动，Locator 体系不变。
   定位 Locator = {chapter, para, offset}，chunk 由 para 推导、不单独持久化。
   ============================================================ */
import { clamp } from '../util/core.js';

export const MAX_CHUNK_CHARS = 8000;   // 单块最大字数
const FALLBACK_MIN_CHARS = 2500;       // 超过此字数若仍只有 1 页 → 判定列布局失效

/* 按段落把章节切成若干块，块内段落连续；标题只进第一块 */
function buildChunks(paragraphs, title) {
  const chunks = [];
  if (!paragraphs.length) return [{ start: 0, end: -1 }];
  let start = 0;
  let chars = title ? title.length + 4 : 0;
  for (let i = 0; i < paragraphs.length; i++) {
    const len = paragraphs[i].length + 1;
    if (i > start && chars + len > MAX_CHUNK_CHARS) {
      chunks.push({ start, end: i - 1 });
      start = i;
      chars = 0;
    }
    chars += len;
  }
  chunks.push({ start, end: paragraphs.length - 1 });
  return chunks;
}

export class Pager {
  constructor(viewport, column, handlers = {}) {
    this.viewport = viewport;
    this.column = column;
    this.onChange = handlers.onChange || (() => {});
    this.onVertical = handlers.onVertical || (() => {});

    this.chapter = null;
    this.title = '';
    this.paragraphs = null;
    this.chunks = null;
    this.ci = 0;              // 当前块下标
    this.page = 0;
    this.pages = 1;
    this.stride = 0;          // 单页宽度（= viewport.clientWidth）
    this.vertical = false;
    this.paraPage = null;     // Map<para, page>，仅当前块
    this.pageFirstPara = null; // Int32Array(pages)，每页第一个段落下标

    this._raf = 0;
    this._scrollTimer = 0;
    this._turnTimer = 0;
    this._busy = false;
    this.annotations = null;  // 当前章注释（只驻留当前章，切章由 reader 清空）
    this._onScroll = () => {
      if (!this.vertical || this._busy) return;
      if (this._scrollTimer) return;
      this._scrollTimer = setTimeout(() => {
        this._scrollTimer = 0;
        if (!this.vertical) return;
        const vp = this.viewport;
        // 到达块底部 → 自动衔接下一块；到达顶部 → 衔接上一块
        if (vp.scrollTop + vp.clientHeight >= vp.scrollHeight - 6 && this.ci < this.chunks.length - 1) {
          this._renderChunk(this.ci + 1, this.chunks[this.ci + 1].start, 'start').then(() => this.emit());
          return;
        }
        this.emit();
      }, 160);
    };
    this.viewport.addEventListener('scroll', this._onScroll, { passive: true });
  }

  /* ---------- 生命周期 ---------- */

  /* 销毁旧章 DOM 与引用（切章前必须先调用，保证内存里不残留两章正文） */
  clear() {
    if (this._raf) { cancelAnimationFrame(this._raf); this._raf = 0; }
    this._clearTurn();
    this.paragraphs = null;
    this.chunks = null;
    this.paraPage = null;
    this.pageFirstPara = null;
    this.annotations = null;
    this.page = 0;
    this.pages = 1;
    this.ci = 0;
    this.chapter = null;
    this.title = '';
    this.column.replaceChildren();
    this.column.style.transform = 'translateX(0px)';
    if (this.vertical) {
      this.vertical = false;
      this.viewport.classList.remove('mode-scroll');
    }
    this.viewport.scrollTop = 0;
  }

  async load(chapter, title, paragraphs, target = {}) {
    this.chapter = chapter;
    this.title = title || '';
    this.paragraphs = Array.isArray(paragraphs) ? paragraphs : [];
    this.chunks = buildChunks(this.paragraphs, this.title);
    const para = clamp(Number(target.para) || 0, 0, Math.max(0, this.paragraphs.length - 1));
    const ci = this.chunkIndexForPara(para);
    const landAt = target.landAt === 'end' ? 'end' : 'start';
    await this._renderChunk(ci, para, landAt);
    this.emit();
  }

  /* ---------- 查询 ---------- */

  chunkIndexForPara(para) {
    for (let i = 0; i < this.chunks.length; i++) {
      if (para <= this.chunks[i].end) return i;
    }
    return this.chunks.length - 1;
  }

  pageForPara(para) {
    if (this.vertical) return 0;
    if (this.paraPage && this.paraPage.has(para)) return this.paraPage.get(para);
    const chunk = this.chunks && this.chunks[this.ci];
    if (!chunk) return 0;
    if (para < chunk.start) return 0;
    if (para > chunk.end) return this.pages - 1;
    return clamp(this.paraPage && this.paraPage.size ? Math.max(...this.paraPage.values()) : 0, 0, this.pages - 1);
  }

  locatePara() {
    if (this.vertical) {
      const vpTop = this.viewport.getBoundingClientRect().top + 6;
      for (const node of this.column.children) {
        const raw = node.getAttribute && node.getAttribute('data-i');
        if (raw === null) continue;
        const rect = node.getBoundingClientRect();
        if (rect.bottom > vpTop) return Number(raw);
      }
      const chunk = this.chunks && this.chunks[this.ci];
      return chunk ? chunk.start : 0;
    }
    if (this.pageFirstPara && this.pageFirstPara.length) {
      const p = this.pageFirstPara[clamp(this.page, 0, this.pageFirstPara.length - 1)];
      if (p >= 0) return p;
    }
    return this.paragraphs && this.paragraphs.length ? 0 : 0;
  }

  emit() {
    if (!this.paragraphs) return;
    this.onChange({
      chapter: this.chapter,
      para: this.locatePara(),
      offset: 0,
      page: this.page,
      pages: this.pages,
      chunk: this.ci,
      chunks: this.chunks ? this.chunks.length : 1,
      vertical: this.vertical,
    });
  }

  /* ---------- 翻页（只改 transform；按钮/键盘翻页带 180ms 过渡） ---------- */
  _clearTurn() {
    if (this._turnTimer) { clearTimeout(this._turnTimer); this._turnTimer = 0; }
    if (this.column) this.column.classList.remove('is-turning');
  }

  applyTransform(animate) {
    if (this.vertical) return;
    if (animate) {
      this.column.classList.add('is-turning');
      if (this._turnTimer) clearTimeout(this._turnTimer);
      this._turnTimer = setTimeout(() => {
        this._turnTimer = 0;
        this.column.classList.remove('is-turning');
      }, 220);
    } else {
      this._clearTurn();
    }
    this.column.style.transform = 'translateX(' + (-this.page * this.stride) + 'px)';
  }

  async next(opts) {
    const animate = !!(opts && opts.animate);
    if (!this.paragraphs || !this.chunks) return false;
    if (this.vertical) {
      const vp = this.viewport;
      const before = vp.scrollTop;
      vp.scrollTop = before + Math.round(vp.clientHeight * 0.86);
      if (vp.scrollTop === before) {
        if (this.ci < this.chunks.length - 1) {
          await this._renderChunk(this.ci + 1, this.chunks[this.ci + 1].start, 'start');
          this.emit();
          return true;
        }
        return false;
      }
      this.emit();
      return true;
    }
    if (this.page < this.pages - 1) {
      this.page += 1;
      this.applyTransform(animate);
      this.emit();
      return true;
    }
    if (this.ci < this.chunks.length - 1) {
      await this._renderChunk(this.ci + 1, this.chunks[this.ci + 1].start, 'start');
      this.emit();
      return true;
    }
    return false;
  }

  async prev(opts) {
    const animate = !!(opts && opts.animate);
    if (!this.paragraphs || !this.chunks) return false;
    if (this.vertical) {
      const vp = this.viewport;
      const before = vp.scrollTop;
      vp.scrollTop = Math.max(0, before - Math.round(vp.clientHeight * 0.86));
      if (vp.scrollTop === before) {
        if (this.ci > 0) {
          await this._renderChunk(this.ci - 1, this.chunks[this.ci - 1].end, 'end');
          this.emit();
          return true;
        }
        return false;
      }
      this.emit();
      return true;
    }
    if (this.page > 0) {
      this.page -= 1;
      this.applyTransform(animate);
      this.emit();
      return true;
    }
    if (this.ci > 0) {
      await this._renderChunk(this.ci - 1, this.chunks[this.ci - 1].end, 'end');
      this.emit();
      return true;
    }
    return false;
  }

  /* 跳到指定段落（同章内，自动切块） */
  async gotoPara(para) {
    if (!this.paragraphs) return;
    const p = clamp(Number(para) || 0, 0, Math.max(0, this.paragraphs.length - 1));
    const ci = this.chunkIndexForPara(p);
    if (this.vertical) {
      if (ci !== this.ci) await this._renderChunk(ci, p, 'start');
      this._scrollToPara(p);
      this.emit();
      return;
    }
    if (ci !== this.ci || !this.paraPage || !this.paraPage.has(p)) {
      await this._renderChunk(ci, p, 'start');
    } else {
      this.page = clamp(this.pageForPara(p), 0, this.pages - 1);
      this.applyTransform();
    }
    this.emit();
  }

  /* 跳到指定页（URL 深链 p= 的页码口径，块内页，越界自动夹取；只改 transform） */
  goToPage(page) {
    if (!this.paragraphs || this.vertical) return;
    this.page = clamp(Number(page) || 0, 0, this.pages - 1);
    this.applyTransform();
    this.emit();
  }

  /* 重新测量并回到同一阅读位置（resize / 字号变化后调用） */
  async relocate() {
    if (!this.paragraphs) return;
    const para = this.locatePara();
    if (this.vertical) {
      await this._waitFrames();
      this._scrollToPara(para);
      this.emit();
      return;
    }
    await this._waitFrames();
    const m = this._measure();
    this._applyMeasure(m);
    this.page = clamp(this.pageForPara(para), 0, this.pages - 1);
    this.applyTransform();
    this.emit();
  }

  destroy() {
    this.viewport.removeEventListener('scroll', this._onScroll);
    if (this._raf) { cancelAnimationFrame(this._raf); this._raf = 0; }
    if (this._scrollTimer) { clearTimeout(this._scrollTimer); this._scrollTimer = 0; }
    this.clear();
  }

  /* ---------- 注释高亮（契约 7.5 / 7.6） ----------
     渲染规则：<mark class="ann ann-<color>" data-ann="<id>"> 只包裹原文文本节点，
     不改文本内容；背景色与笔记双下划线不占布局空间，因此不影响分页测量。
     切章/换块/改字号/换主题后整章重建：先拆旧 mark 恢复纯文本，再按当前注释列表重包。 */

  setAnnotations(list) {
    this.annotations = Array.isArray(list) ? list : [];
    this._applyAnnotations();
  }

  reapplyAnnotations() { this._applyAnnotations(); }

  _applyAnnotations() {
    const col = this.column;
    if (!col) return;
    // 1) 拆除旧 mark（幂等）并合并相邻文本节点
    const marks = col.querySelectorAll('mark.ann');
    if (marks.length) {
      for (const m of marks) {
        const parent = m.parentNode;
        if (!parent) continue;
        while (m.firstChild) parent.insertBefore(m.firstChild, m);
        parent.removeChild(m);
      }
      for (const p of col.children) if (p.normalize) p.normalize();
    }
    const list = this.annotations;
    if (!list || !list.length) return;
    // 2) 按段分组，只处理当前块里存在的段落
    const byPara = new Map();
    for (const a of list) {
      if (!a || !Number.isInteger(a.para)) continue;
      let arr = byPara.get(a.para);
      if (!arr) { arr = []; byPara.set(a.para, arr); }
      arr.push(a);
    }
    for (const [para, arr] of byPara) {
      const node = col.querySelector('[data-i="' + para + '"]');
      if (node) this._wrapPara(node, arr);
    }
  }

  _wrapPara(node, arr) {
    const text = node.textContent || '';
    const len = text.length;
    if (!len) return;
    const items = [];
    for (const a of arr) {
      const item = {
        id: String(a.id || ''),
        color: a.color || 'yellow',
        hasNote: typeof a.note === 'string' && a.note.trim().length > 0,
        s: Math.max(0, Number(a.start_offset) || 0),
        e: Math.min(len, Number(a.end_offset) || 0),
      };
      if (item.s < item.e) items.push(item);
    }
    if (!items.length) return;
    items.sort((x, y) => x.s - y.s || x.e - y.e);
    const frag = document.createDocumentFragment();
    let cursor = 0;
    for (const it of items) {
      if (it.e <= cursor) continue;      // 完全被前一个注释覆盖
      if (it.s < cursor) it.s = cursor;  // 部分重叠：裁掉重叠部分，避免嵌套 mark
      if (it.s > cursor) frag.appendChild(document.createTextNode(text.slice(cursor, it.s)));
      const mark = document.createElement('mark');
      mark.className = 'ann ann-' + it.color + (it.hasNote ? ' ann-note' : '');
      mark.setAttribute('data-ann', it.id);
      mark.tabIndex = 0;
      mark.setAttribute('role', 'button');
      mark.setAttribute('aria-label', (it.hasNote ? '查看批注：' : '编辑高亮：') + text.slice(it.s, it.e));
      mark.setAttribute('aria-haspopup', 'dialog');
      mark.textContent = text.slice(it.s, it.e);
      frag.appendChild(mark);
      cursor = it.e;
    }
    if (cursor < len) frag.appendChild(document.createTextNode(text.slice(cursor)));
    node.replaceChildren(frag);
  }

  /* ---------- 内部：渲染 / 测量 ---------- */

  async _renderChunk(ci, focusPara, landAt) {
    this.ci = ci;
    this._busy = true;
    this._clearTurn();
    this.column.replaceChildren();
    const chunk = this.chunks[ci];
    const frag = document.createDocumentFragment();
    if (ci === 0 && this.title) {
      const h = document.createElement('h2');
      h.className = 'ch-title';
      h.textContent = this.title;
      frag.appendChild(h);
    }
    for (let i = chunk.start; i <= chunk.end; i++) {
      const p = document.createElement('p');
      p.id = 'p' + i;
      p.setAttribute('data-i', String(i));
      p.textContent = this.paragraphs[i];
      frag.appendChild(p);
    }
    this.column.appendChild(frag);
    this._applyAnnotations();
    this.column.style.transform = 'translateX(0px)';
    await this._waitFrames();

    if (!this.vertical) {
      const m = this._measure();
      // 判定列布局失效：长文本仍挤在 1 页 → 切垂直滚动
      if (m.chars > FALLBACK_MIN_CHARS && m.pages <= 1) {
        await this._enableVertical(focusPara);
        this._busy = false;
        return;
      }
      this._applyMeasure(m);
      if (landAt === 'end') this.page = this.pages - 1;
      else if (Number.isInteger(focusPara)) this.page = clamp(this.pageForPara(focusPara), 0, this.pages - 1);
      else this.page = clamp(this.page, 0, this.pages - 1);
      this.applyTransform();
    } else {
      this._scrollToPara(focusPara);
    }
    this._busy = false;
  }

  _waitFrames() {
    return new Promise((resolve) => {
      if (this._raf) cancelAnimationFrame(this._raf);
      this._raf = requestAnimationFrame(() => {
        this._raf = requestAnimationFrame(() => {
          this._raf = 0;
          resolve();
        });
      });
    });
  }

  _chunkChars(chunk) {
    if (!chunk || chunk.end < chunk.start) return 0;
    let n = 0;
    for (let i = chunk.start; i <= chunk.end; i++) n += this.paragraphs[i].length;
    return n;
  }

  /* 只读一次布局：页数 + 段→页映射。翻页阶段不再读 layout */
  _measure() {
    const vw = this.viewport.clientWidth || 1;
    this.stride = vw;
    const colRect = this.column.getBoundingClientRect();
    const pageMap = new Map();
    let maxRight = 0;
    const children = this.column.children;
    for (let k = 0; k < children.length; k++) {
      const node = children[k];
      const raw = node.getAttribute('data-i');
      const rect = node.getBoundingClientRect();
      const left = rect.left - colRect.left;
      if (raw !== null) pageMap.set(Number(raw), Math.round(left / vw));
      const right = rect.right - colRect.left;
      if (right > maxRight) maxRight = right;
    }
    const chunk = this.chunks[this.ci];
    const chars = this._chunkChars(chunk);
    let pages = Math.max(1, Math.ceil((maxRight - 1) / vw));
    const sw = this.column.scrollWidth || 0;
    const swPages = Math.max(1, Math.round(sw / vw));
    if (swPages > pages && swPages <= pages + 2) pages = swPages;
    return { pages, pageMap, chars };
  }

  _applyMeasure(m) {
    this.pages = m.pages;
    this.paraPage = m.pageMap;
    const chunk = this.chunks[this.ci];
    const chunkStart = chunk ? chunk.start : 0;
    const first = new Int32Array(this.pages).fill(-1);
    for (let i = chunkStart; i <= chunk.end; i++) {
      const pg = m.pageMap.get(i);
      if (pg === undefined) continue;
      const clamped = clamp(pg, 0, this.pages - 1);
      if (first[clamped] < 0) first[clamped] = i;
    }
    let carry = chunkStart;
    for (let pg = 0; pg < this.pages; pg++) {
      if (first[pg] < 0) first[pg] = carry;
      else carry = first[pg];
    }
    this.pageFirstPara = first;
  }

  async _enableVertical(focusPara) {
    this.vertical = true;
    this.viewport.classList.add('mode-scroll');
    this.column.style.transform = 'none';
    this.paraPage = null;
    this.pageFirstPara = null;
    this.pages = 1;
    this.page = 0;
    await this._waitFrames();
    this._scrollToPara(focusPara);
    this.onVertical();
  }

  _scrollToPara(para) {
    const node = this.column.querySelector('[data-i="' + para + '"]');
    if (!node) return;
    const vpRect = this.viewport.getBoundingClientRect();
    const nodeRect = node.getBoundingClientRect();
    this.viewport.scrollTop += nodeRect.top - vpRect.top - 12;
  }
}
