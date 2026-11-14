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
import { resolveImage } from '../util/local-media.js';

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
    this.illustrations = [];
    this.releaseImages = [];
    this.paragraphFormats = new Map();
    this.chunks = null;
    this.ci = 0;              // 当前块下标
    this.page = 0;
    this.pages = 1;
    this.stride = 0;          // 单页宽度（= viewport.clientWidth）
    this.vertical = false;
    this.preferScroll = false;
    this.paraPage = null;     // Map<para, page>，仅当前块
    this.pageFirstPara = null; // Int32Array(pages)，每页第一个段落下标
    this.pageLocations = [];
    this.location = null;

    this._frameWaits = new Map();
    this._renderEpoch = 0;
    this._scrollTimer = 0;
    this._scrollAnchor = null;
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
        // gotoPara() can target a character in the middle of a visible line.
        // Its native scroll event must not replace that exact anchor with the
        // first visible character measured from the viewport.
        if (this._scrollAnchor !== null && Math.abs(vp.scrollTop - this._scrollAnchor) < .5) return;
        this._scrollAnchor = null;
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
    this._renderEpoch++;
    if (this._scrollTimer) { clearTimeout(this._scrollTimer); this._scrollTimer = 0; }
    this._scrollAnchor = null;
    for (const release of this.releaseImages) release(); this.releaseImages = [];
    for (const [frame, resolve] of this._frameWaits) { cancelAnimationFrame(frame); resolve(); }
    this._frameWaits.clear();
    this._busy = false;
    this._clearTurn();
    this.paragraphs = null;
    this.chunks = null;
    this.paraPage = null;
    this.pageFirstPara = null;
    this.pageLocations = [];
    this.location = null;
    this.annotations = null;
    this.illustrations = [];
    this.paragraphFormats = new Map();
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

  async load(chapter, title, paragraphs, target = {}, illustrations = [], paragraphFormats = []) {
    this.chapter = chapter;
    this.title = title || '';
    this.paragraphs = Array.isArray(paragraphs) ? paragraphs : [];
    this.illustrations = Array.isArray(illustrations) ? illustrations : [];
    this.paragraphFormats = new Map((Array.isArray(paragraphFormats) ? paragraphFormats : [])
      .filter(item => item && Number.isInteger(item.para) && Array.isArray(item.formats))
      .map(item => [item.para, item.formats]));
    this.chunks = buildChunks(this.paragraphs, this.title);
    const para = clamp(Number(target.para) || 0, 0, Math.max(0, this.paragraphs.length - 1));
    const ci = this.chunkIndexForPara(para);
    const landAt = target.landAt === 'end' ? 'end' : 'start';
    if (!(await this._renderChunk(ci, para, landAt))) return;
    this._restoreOffset(para, target.offset);
    this.emit(landAt === 'end' ? null : { para, offset: Math.max(0, Number(target.offset) || 0) });
  }

  // Synchronised automatic reading needs a continuous scroll surface.  This
  // preference survives chapter clear/load while ordinary manual reading can
  // restore the original paged layout at the same character locator.
  async setScrollReading(enabled) {
    const wanted = !!enabled;
    this.preferScroll = wanted;
    if (!this.paragraphs || this.vertical === wanted) return;
    const location = this.location || { para: this.locatePara(), offset: this.locateOffset() };
    if (wanted) await this._enableVertical(location.para);
    else {
      this.vertical = false;
      this.viewport.classList.remove('mode-scroll');
      await this._renderChunk(this.ci, location.para, 'start');
    }
    this._restoreOffset(location.para, location.offset);
    this.emit(location);
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

  /* 页首坐标在布局测量时计算，翻页只读取缓存，不在动画中测量字符。 */
  locateOffset() {
    if (!this.vertical) return this.pageLocations[this.page]?.offset || 0;
    const node = this.column.querySelector('[data-i="' + this.locatePara() + '"]');
    if (!node) return 0;
    const text = this._textNodes(node);
    const top = this.viewport.getBoundingClientRect().top + 6;
    const offset = this._firstOffset(text, (rect) => rect.bottom > top);
    return Array.from(node.textContent.slice(0, offset)).length;
  }

  emit(anchor = null) {
    if (!this.paragraphs) return;
    this.location = anchor || { para: this.locatePara(), offset: this.locateOffset() };
    this._scrollAnchor = anchor && this.vertical ? this.viewport.scrollTop : null;
    this.onChange({
      chapter: this.chapter,
      ...this.location,
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
  async gotoPara(para, offset = 0) {
    if (!this.paragraphs) return;
    const p = clamp(Number(para) || 0, 0, Math.max(0, this.paragraphs.length - 1));
    const ci = this.chunkIndexForPara(p);
    if (this.vertical) {
      if (ci !== this.ci && !(await this._renderChunk(ci, p, 'start'))) return;
      this._scrollToPara(p);
      this._restoreOffset(p, offset);
      this.emit({ para: p, offset: Math.max(0, Number(offset) || 0) });
      return;
    }
    if (ci !== this.ci || !this.paraPage || !this.paraPage.has(p)) {
      if (!(await this._renderChunk(ci, p, 'start'))) return;
    } else {
      this.page = clamp(this.pageForPara(p), 0, this.pages - 1);
      this.applyTransform();
    }
    this._restoreOffset(p, offset);
    this.emit({ para: p, offset: Math.max(0, Number(offset) || 0) });
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
    const epoch = this._renderEpoch;
    // resize/字号变化已经改变 DOM；使用上次 emit 保存的字符锚点。
    const { para, offset } = this.location || { para: this.locatePara(), offset: 0 };
    if (this.vertical) {
      await this._waitFrames();
      if (epoch !== this._renderEpoch) return;
      this._scrollToPara(para);
      this._restoreOffset(para, offset);
      this.emit({ para, offset });
      return;
    }
    await this._waitFrames();
    if (epoch !== this._renderEpoch) return;
    const m = this._measure();
    this._applyMeasure(m);
    this.page = clamp(this.pageForPara(para), 0, this.pages - 1);
    this.applyTransform();
    this._restoreOffset(para, offset);
    this.emit({ para, offset });
  }

  destroy() {
    this.viewport.removeEventListener('scroll', this._onScroll);
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
    const formats = (this.paragraphFormats.get(Number(node.dataset.i)) || [])
      .filter(item => item && Number.isFinite(item.start) && Number.isFinite(item.end) && item.start < item.end);
    const boundaries = new Set([0, len]);
    for (const item of items) boundaries.add(item.s), boundaries.add(item.e);
    for (const item of formats) boundaries.add(Math.max(0, Math.min(len, item.start))), boundaries.add(Math.max(0, Math.min(len, item.end)));
    const points = [...boundaries].sort((a, b) => a - b);
    const makeInline = (kind, child) => {
      const tag = ({bold: 'strong', strong: 'strong', italic: 'em', em: 'em', underline: 'u', u: 'u', strike: 's', s: 's', code: 'code'}[kind]) || 'span';
      const value = document.createElement(tag);
      if (tag === 'span') value.className = 'epub-inline-' + String(kind).replace(/[^a-z0-9_-]/gi, '');
      value.appendChild(child); return value;
    };
    const annotationAt = offset => items.find(item => item.s <= offset && item.e > offset);
    for (let point = 0; point < points.length - 1; point++) {
      const start = points[point], end = points[point + 1];
      if (end <= start) continue;
      let child = document.createTextNode(text.slice(start, end));
      const activeFormats = formats.filter(item => item.start <= start && item.end >= end);
      for (let i = activeFormats.length - 1; i >= 0; i--) child = makeInline(activeFormats[i].kind, child);
      const annotation = annotationAt(start);
      if (annotation) {
        const mark = document.createElement('mark');
        mark.className = 'ann ann-' + annotation.color + (annotation.hasNote ? ' ann-note' : '');
        mark.setAttribute('data-ann', annotation.id); mark.tabIndex = 0; mark.setAttribute('role', 'button');
        mark.setAttribute('aria-label', (annotation.hasNote ? '查看批注：' : '编辑高亮：') + text.slice(annotation.s, annotation.e));
        mark.setAttribute('aria-haspopup', 'dialog'); mark.appendChild(child); child = mark;
      }
      frag.appendChild(child);
    }
    node.replaceChildren(frag);
  }

  _renderParagraph(node, text, formats) {
    if (!Array.isArray(formats) || !formats.length) { node.textContent = text; return; }
    const boundaries = new Set([0, text.length]);
    for (const item of formats) {
      if (!item || !Number.isFinite(item.start) || !Number.isFinite(item.end)) continue;
      boundaries.add(Math.max(0, Math.min(text.length, item.start)));
      boundaries.add(Math.max(0, Math.min(text.length, item.end)));
    }
    const points = [...boundaries].sort((a, b) => a - b), frag = document.createDocumentFragment();
    const tags = {bold: 'strong', strong: 'strong', italic: 'em', em: 'em', underline: 'u', u: 'u', strike: 's', s: 's', code: 'code'};
    for (let i = 0; i < points.length - 1; i++) {
      const start = points[i], end = points[i + 1]; if (end <= start) continue;
      let child = document.createTextNode(text.slice(start, end));
      const active = formats.filter(item => item && item.start <= start && item.end >= end);
      for (let j = active.length - 1; j >= 0; j--) {
        const tag = tags[active[j].kind] || 'span', wrap = document.createElement(tag);
        if (tag === 'span') wrap.className = 'epub-inline-' + String(active[j].kind).replace(/[^a-z0-9_-]/gi, '');
        wrap.appendChild(child); child = wrap;
      }
      frag.appendChild(child);
    }
    node.replaceChildren(frag);
  }

  /* ---------- 内部：渲染 / 测量 ---------- */

  async _renderChunk(ci, focusPara, landAt) {
    const epoch = ++this._renderEpoch;
    if (!this.chunks?.[ci]) return false;
    for (const release of this.releaseImages) release(); this.releaseImages = [];
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
    const atParagraph = new Map();
    for (const item of this.illustrations) {
      if (!item || typeof item.url !== 'string') continue;
      const anchor = Number.isInteger(item.before_para) ? clamp(item.before_para, 0, this.paragraphs.length) : this.paragraphs.length;
      if (!atParagraph.has(anchor)) atParagraph.set(anchor, []);
      atParagraph.get(anchor).push(item);
    }
    const appendIllustrations = async (anchor) => {
      const items = atParagraph.get(anchor);
      if (!items?.length) return true;
      atParagraph.delete(anchor);
      const gallery = document.createElement('div'); gallery.className = 'epub-illustrations'; gallery.setAttribute('aria-label', '章节插图');
      gallery.dataset.beforePara = String(anchor);
      for (const item of items) {
        const bookId = item.url.match(/^\/api\/books\/([^/]+)\//)?.[1];
        const media = bookId ? await resolveImage(decodeURIComponent(bookId), item.url) : { url: item.url, release: () => {} };
        if (epoch !== this._renderEpoch) { media.release(); return false; }
        this.releaseImages.push(media.release);
        const image = document.createElement('img'); image.src = media.url; image.alt = item.alt || '章节插图'; image.referrerPolicy = 'no-referrer';
        image.addEventListener('load', () => { if (epoch === this._renderEpoch) this.relocate(); }, { once: true });
        gallery.appendChild(image);
      }
      if (gallery.childElementCount) frag.appendChild(gallery);
      return true;
    };
    for (let i = chunk.start; i <= chunk.end; i++) {
      if (!(await appendIllustrations(i))) return false;
      const p = document.createElement('p');
      p.id = 'p' + i;
      p.setAttribute('data-i', String(i));
      this._renderParagraph(p, this.paragraphs[i], this.paragraphFormats.get(i));
      frag.appendChild(p);
    }
    if (ci === this.chunks.length - 1 && !(await appendIllustrations(this.paragraphs.length))) return false;
    this.column.appendChild(frag);
    this._applyAnnotations();
    this.column.style.transform = 'translateX(0px)';
    await this._waitFrames();
    if (epoch !== this._renderEpoch) return false;

    if (this.preferScroll && !this.vertical) {
      await this._enableVertical(focusPara, epoch);
      if (epoch !== this._renderEpoch) return false;
    }
    if (!this.vertical) {
      const m = this._measure();
      // 判定列布局失效：长文本仍挤在 1 页 → 切垂直滚动
      if (m.chars > FALLBACK_MIN_CHARS && m.pages <= 1) {
        await this._enableVertical(focusPara, epoch);
        if (epoch !== this._renderEpoch) return false;
        this._busy = false;
        return true;
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
    return true;
  }

  _waitFrames() {
    return new Promise((resolve) => {
      const first = requestAnimationFrame(() => {
        this._frameWaits.delete(first);
        const second = requestAnimationFrame(() => {
          this._frameWaits.delete(second);
          resolve();
        });
        this._frameWaits.set(second, resolve);
      });
      this._frameWaits.set(first, resolve);
    });
  }

  _chunkChars(chunk) {
    if (!chunk || chunk.end < chunk.start) return 0;
    let n = 0;
    for (let i = chunk.start; i <= chunk.end; i++) n += this.paragraphs[i].length;
    return n;
  }

  /* 构建段落起始页与每页首字符坐标，跨页的段落仍属于当前页。 */
  _measure() {
    const vw = this.viewport.clientWidth || 1;
    this.stride = vw;
    const colRect = this.column.getBoundingClientRect();
    const pageMap = new Map(), locations = [];
    let maxRight = 0;
    const pageOf = (rect) => Math.max(0, Math.floor((rect.left - colRect.left + .5) / vw));
    for (const node of this.column.children) {
      const rect = node.getBoundingClientRect();
      maxRight = Math.max(maxRight, rect.right - colRect.left);
      const raw = node.getAttribute('data-i');
      if (raw === null) continue;
      const para = Number(raw), text = this._textNodes(node);
      if (!text.length) continue;
      const first = this._charRectFromText(text, 0), last = this._charRectFromText(text, text.length - 1);
      if (!first || !last) continue;
      const start = pageOf(first), end = pageOf(last);
      pageMap.set(para, start);
      for (let page = start; page <= end; page++) {
        if (locations[page]) continue;
        const offset = page === start ? 0 : this._firstOffset(text, (r) => pageOf(r) >= page);
        locations[page] = { para, offset: Array.from(node.textContent.slice(0, offset)).length };
      }
    }
    const chars = this._chunkChars(this.chunks[this.ci]);
    const pages = Math.max(1, locations.length, Math.ceil((maxRight - 1) / vw));
    return { pages, pageMap, chars, locations };
  }

  _applyMeasure(m) {
    this.pages = m.pages;
    this.paraPage = m.pageMap;
    let carry = { para: this.chunks[this.ci].start, offset: 0 };
    this.pageLocations = Array.from({ length: this.pages }, (_, page) => {
      carry = m.locations[page] || carry;
      return carry;
    });
    this.pageFirstPara = Int32Array.from(this.pageLocations, (loc) => loc.para);
  }

  async _enableVertical(focusPara, epoch = this._renderEpoch) {
    this.vertical = true;
    this.viewport.classList.add('mode-scroll');
    this.column.style.transform = 'none';
    this.paraPage = null;
    this.pageFirstPara = null;
    this.pages = 1;
    this.page = 0;
    await this._waitFrames();
    if (epoch !== this._renderEpoch) return;
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

  _textNodes(node) {
    const result = [];
    const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
    let current;
    let start = 0;
    while ((current = walker.nextNode())) {
      const length = current.nodeValue ? current.nodeValue.length : 0;
      if (!length) continue;
      result.push({ node: current, start, end: start + length });
      start += length;
    }
    return { nodes: result, length: start };
  }

  _pointAt(text, offset) {
    const pos = clamp(Number(offset) || 0, 0, text.length);
    for (const item of text.nodes) {
      if (pos <= item.end) return [item.node, Math.max(0, pos - item.start)];
    }
    const last = text.nodes[text.nodes.length - 1];
    return last ? [last.node, last.node.nodeValue.length] : null;
  }

  _charRectFromText(text, offset) {
    if (!text.length) return null;
    const start = clamp(Number(offset) || 0, 0, text.length - 1);
    const from = this._pointAt(text, start);
    const to = this._pointAt(text, start + 1);
    const range = document.createRange();
    range.setStart(from[0], from[1]);
    range.setEnd(to[0], to[1]);
    return range.getClientRects()[0] || null;
  }

  _firstOffset(text, visible) {
    let low = 0, high = text.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      const rect = this._charRectFromText(text, mid);
      if (rect && visible(rect)) high = mid;
      else low = mid + 1;
    }
    return low;
  }

  _restoreOffset(para, offset) {
    const node = this.column.querySelector('[data-i="' + para + '"]');
    if (!node) return;
    const text = this._textNodes(node);
    // API offset 与 Python len 一致，按 Unicode 字符计数；DOM Range 使用 UTF-16。
    const wanted = Math.max(0, Number(offset) || 0);
    if (!wanted) return;
    const units = Array.from(node.textContent).slice(0, wanted).join('').length;
    const rect = this._charRectFromText(text, units);
    if (!rect) return;
    if (this.vertical) {
      const vp = this.viewport.getBoundingClientRect();
      this.viewport.scrollTop += rect.top - vp.top - 12;
      return;
    }
    const col = this.column.getBoundingClientRect();
    const page = clamp(Math.floor((rect.left - col.left) / Math.max(1, this.stride)), 0, this.pages - 1);
    this.page = page;
    this.applyTransform();
  }
}
