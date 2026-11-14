/*
 * Synchronized novel/comic reader.
 * The backend map links every generated panel to a half-open character range
 * in a chapter.  This module owns one cursor for manual, timer and speech
 * modes; the cursor survives mode changes and chapter navigation.
 */
import { el, apiGet, clamp, toast } from '../util/core.js';

const CSS_HREF = new URL('../styles/reading-sync.css', import.meta.url).href;
const SPEEDS = [0.75, 1, 1.25, 1.5, 2];

function ensureStyles() {
  if (document.querySelector('link[data-reading-sync]')) return;
  const link = document.createElement('link');
  link.rel = 'stylesheet'; link.href = CSS_HREF; link.dataset.readingSync = '1';
  document.head.appendChild(link);
}

export function chapterCodepointOffsets(paragraphs) {
  const offsets = [];
  let cursor = 0;
  for (const paragraph of paragraphs || []) {
    offsets.push(cursor);
    cursor += Array.from(String(paragraph || '')).length + 1;
  }
  return { offsets, length: Math.max(0, cursor - (paragraphs?.length ? 1 : 0)) };
}

export function positionForCodepoint(value, paragraphs) {
  const { offsets } = chapterCodepointOffsets(paragraphs);
  const total = chapterCodepointOffsets(paragraphs).length;
  const wanted = clamp(Number(value) || 0, 0, total);
  let para = Math.max(0, (paragraphs || []).length - 1);
  for (let i = 0; i < offsets.length; i++) {
    const end = offsets[i] + Array.from(String(paragraphs[i] || '')).length;
    if (wanted <= end) { para = i; break; }
  }
  return { para, offset: Math.max(0, wanted - (offsets[para] || 0)) };
}

function codepointForPosition(position, paragraphs) {
  if (position && typeof position === 'object') {
    const para = Number.isInteger(position.para) ? position.para : Number(position.paragraph);
    const offset = Number(position.offset) || 0;
    const { offsets } = chapterCodepointOffsets(paragraphs);
    const index = clamp(Number.isInteger(para) ? para : 0, 0, Math.max(0, (paragraphs || []).length - 1));
    return (offsets[index] || 0) + Math.max(0, offset);
  }
  return Math.max(0, Number(position) || 0);
}

export function normalizeReadingMap(raw) {
  const values = Array.isArray(raw) ? raw : Array.isArray(raw?.panels) ? raw.panels : [];
  return values.map((item, index) => ({
    ...item,
    index,
    chapter: Number(item.chapter ?? item.chapter_id ?? 0),
    start: item.start ?? item.start_offset ?? item.source_start ?? item.start_char ?? item.source_quote_start ?? 0,
    end: item.end ?? item.end_offset ?? item.source_end ?? item.end_char ?? item.source_quote_end ?? item.start ?? 0,
  })).filter(item => Number.isFinite(item.chapter));
}

function imageFor(item) {
  // A panel URL is preferred; page_url is only a fallback for maps that also
  // provide a crop rectangle (the server may expose the page as its source).
  return item.url || item.image_url || item.image || item.panel_url || item.page_url || '';
}

export function panelRange(item, paragraphs) {
  const start = codepointForPosition(item.start, paragraphs);
  let end = codepointForPosition(item.end, paragraphs);
  // Ranges are half-open.  Old maps sometimes emitted only a start; keep one
  // codepoint visible rather than treating the panel as an empty range.
  if (end <= start) end = start + 1;
  return { start, end };
}

function sliceRange(item, paragraphs, from = null) {
  const text = (paragraphs || []).map(String).join('\n');
  const range = panelRange(item, paragraphs);
  const start = Math.max(range.start, from === null ? range.start : from);
  return { text: Array.from(text).slice(start, range.end).join(''), range };
}

export async function mountReadingSync({ bookId, main, foot, pager, getState, getParagraphs, navigate, onTextProgress }) {
  if (!bookId || !main || !pager) return null;
  ensureStyles();
  const root = el('aside', 'reader-comics reader-sync-comics');
  root.setAttribute('aria-label', '同步漫画');
  const head = el('div', 'reader-comics-head');
  const title = el('h2', null, '同步漫画');
  const status = el('span', 'reading-sync-status', '手动阅读');
  head.append(title, status);
  const note = el('p', 'manga-note', '正在读取漫画分镜映射…');
  const gallery = el('div', 'reader-comics-pages');
  root.append(head, note, gallery); main.appendChild(root); main.classList.add('with-comics');

  const controls = el('div', 'reading-sync-controls');
  const manual = el('button', 'btn btn-tool', '手动'); manual.type = 'button';
  const auto = el('button', 'btn btn-tool', '自动播放'); auto.type = 'button';
  const speech = el('button', 'btn btn-tool', '语音朗读'); speech.type = 'button';
  const pause = el('button', 'btn btn-tool', '暂停'); pause.type = 'button'; pause.hidden = true;
  const rate = el('select', 'reading-sync-rate'); rate.setAttribute('aria-label', '播放速度');
  for (const value of SPEEDS) { const option = el('option', null, value + '×'); option.value = String(value); if (value === 1) option.selected = true; rate.appendChild(option); }
  controls.append(manual, auto, speech, pause, rate);
  const footNode = foot || document.querySelector('.reader-foot');
  footNode?.classList.add('has-reading-sync');
  footNode?.prepend(controls);

  let all = []; let cards = []; let renderedChapter = null; let mode = 'manual'; let speed = 1; let paused = false;
  let timer = 0; let current = null; let cursor = null; let refreshPromise = null; let lastRefresh = 0;
  let speechUtterance = null; let speechBase = 0; let speechEpoch = 0; let playEpoch = 0; let internalNavigation = 0; let destroyed = false;
  let scrollPromise = Promise.resolve();

  function state() { return getState() || { chapter: 0, para: 0, offset: 0 }; }
  function list(chapter) {
    return all.filter(item => item.chapter === Number(chapter)).sort((a, b) => {
      const av = typeof a.start === 'number' ? a.start : Number(a.start?.offset) || 0;
      const bv = typeof b.start === 'number' ? b.start : Number(b.start?.offset) || 0;
      return av - bv || a.index - b.index;
    });
  }
  function cardFor(item) { return cards.find(card => card.dataset.panelIndex === String(item.index)); }
  function stopTimer() { if (timer) { clearTimeout(timer); timer = 0; } }
  function stopSpeech() { speechEpoch++; if (typeof speechSynthesis !== 'undefined') speechSynthesis.cancel(); speechUtterance = null; }
  function stopPlayback() { playEpoch++; stopTimer(); stopSpeech(); }
  function scrollCard(card) {
    if (!card) return;
    // Keep the right-hand pane as the scroll owner.  scrollIntoView() may
    // scroll the whole reader page when the two panes are nested on mobile.
    const top = Number(card.offsetTop);
    if (Number.isFinite(top)) {
      const target = Math.max(0, top - Math.max(0, root.clientHeight - card.offsetHeight) / 2);
      root.scrollTop = target;
    }
  }
  function setButtons() {
    manual.classList.toggle('active', mode === 'manual'); auto.classList.toggle('active', mode === 'auto'); speech.classList.toggle('active', mode === 'speech');
    pause.hidden = mode === 'manual'; pause.textContent = paused ? '继续' : '暂停';
    status.textContent = mode === 'manual' ? '手动阅读' : mode === 'auto' ? '自动播放 · ' + speed + '×' : '语音朗读 · ' + speed + '×';
  }
  function renderChapter(chapter) {
    renderedChapter = Number(chapter); cards = []; gallery.replaceChildren();
    const items = list(chapter);
    if (!items.length) { note.textContent = all.length ? '本章尚未生成同步漫画。' : '暂无同步漫画，仍可使用自动阅读和语音朗读。'; return; }
    note.textContent = '漫画按小说原文位置同步'; title.textContent = '第 ' + (Number(chapter) + 1) + ' 章 · 同步漫画';
    for (const item of items) {
      const card = el('figure', 'reader-comic-page reading-sync-panel'); card.dataset.panelIndex = String(item.index);
      const imageUrl = imageFor(item); const image = el('img'); image.loading = 'lazy'; image.decoding = 'async'; image.src = imageUrl; image.alt = '第 ' + (item.panel_index ?? item.index + 1) + ' 格';
      if (!imageUrl) image.hidden = true;
      const caption = item.caption || item.dialogue || item.source_quote || '';
      card.append(image, el('figcaption', 'manga-note', caption)); card.addEventListener('click', () => jumpPanel(item));
      gallery.appendChild(card); cards.push(card);
    }
  }
  function cursorFor(chapter, para, offset) {
    return { chapter: Number(chapter), value: codepointForPosition({ para, offset }, getParagraphs()) };
  }
  function updateActive(chapter, para, offset, shouldScroll = true) {
    const value = cursorFor(chapter, para, offset); cursor = value;
    if (renderedChapter !== Number(chapter)) renderChapter(chapter);
    const items = list(chapter); let found = items.find(item => { const range = panelRange(item, getParagraphs()); return value.value >= range.start && value.value < range.end; });
    if (!found && items.length) found = items.reduce((best, item) => Math.abs(value.value - panelRange(item, getParagraphs()).start) < Math.abs(value.value - panelRange(best, getParagraphs()).start) ? item : best, items[0]);
    current = found || null;
    for (const card of cards) card.classList.toggle('is-active', !!found && card.dataset.panelIndex === String(found.index));
    if (found && shouldScroll) scrollCard(cardFor(found));
    return found;
  }
  async function jumpPanel(item) {
    if (!item) return;
    const activeMode = mode;
    if (mode === 'auto') { playEpoch++; stopTimer(); }
    if (mode === 'speech') stopSpeech();
    if (item.chapter !== Number(state().chapter)) await moveTo({ chapter: item.chapter, para: 0, offset: 0 });
    const pos = positionForCodepoint(panelRange(item, getParagraphs()).start, getParagraphs());
    await moveTo({ chapter: item.chapter, para: pos.para, offset: pos.offset });
    updateActive(item.chapter, pos.para, pos.offset);
    if (activeMode === 'speech' && !paused) speak(textItem());
    if (activeMode === 'auto' && !paused) autoStep();
  }
  async function moveTo(target) {
    internalNavigation++;
    try { await navigate(target); }
    finally { internalNavigation--; }
  }
  function scrollMode(enabled) {
    if (typeof pager.setScrollReading !== 'function') return;
    scrollPromise = scrollPromise.catch(() => {}).then(() => {
      if (!destroyed) return pager.setScrollReading(enabled);
    }).catch(() => {}); // A paged fallback remains usable.
    return scrollPromise;
  }
  async function startAuto() {
    const token = playEpoch;
    await scrollMode(true);
    if (!destroyed && token === playEpoch && mode === 'auto' && !paused) autoStep();
  }
  function textItem() {
    // Reading remains continuous in chapters without panels and through gaps
    // between source ranges.  Speech chunks are deliberately short, so a
    // browser without word boundary events still advances only after speech.
    const s = state(); const paragraphs = getParagraphs(); const length = chapterCodepointOffsets(paragraphs).length;
    const start = cursor && cursor.chapter === Number(s.chapter) ? cursor.value : codepointForPosition(s, paragraphs);
    const text = Array.from(paragraphs.map(String).join('\n'));
    let end = Math.min(length, start + 180);
    for (let i = start + 1; i < end; i++) if (/[。！？!?；;\n]/u.test(text[i])) { end = i + 1; break; }
    return { chapter: Number(s.chapter), start, end, index: -1 };
  }
  async function advanceText() {
    const s = state(); const length = chapterCodepointOffsets(getParagraphs()).length;
    if (cursor && cursor.value >= length && Number(s.chapter) < Number(s.chapterCount) - 1) {
      await moveTo({ chapter: Number(s.chapter) + 1, para: 0, offset: 0 });
      return true;
    }
    return !cursor || cursor.value < length;
  }
  async function autoStep() {
    const token = playEpoch;
    stopTimer();
    if (destroyed || token !== playEpoch || mode !== 'auto' || paused) return;
    const hasMoreText = await advanceText();
    if (destroyed || token !== playEpoch || mode !== 'auto' || paused) return;
    if (!hasMoreText) { mode = 'manual'; setButtons(); return; }
    const s = state(); const paragraphs = getParagraphs(); const total = chapterCodepointOffsets(paragraphs).length;
    const at = cursor && cursor.chapter === Number(s.chapter) ? cursor.value : codepointForPosition(s, paragraphs);
    // Advance inside each panel, not just from panel to panel.  Sentence and
    // paragraph positions share this cursor with speech and manual reading.
    const step = Math.max(1, Math.round(4 * speed));
    const next = Math.min(total, at + step); const pos = positionForCodepoint(next, paragraphs);
    await moveTo({ chapter: Number(s.chapter), para: pos.para, offset: pos.offset });
    if (destroyed || token !== playEpoch || mode !== 'auto' || paused) return;
    updateActive(s.chapter, pos.para, pos.offset);
    if (mode === 'auto' && !paused && token === playEpoch) timer = setTimeout(autoStep, 340);
  }
  function speechFor(item, from = null) { const result = sliceRange(item, getParagraphs(), from); return result; }
  function speak(item, from = null) {
    if (!('speechSynthesis' in window)) { toast('当前浏览器不支持语音朗读'); mode = 'manual'; setButtons(); return; }
    stopSpeech(); if (!item) item = textItem();
    const epoch = speechEpoch;
    if (panelRange(item, getParagraphs()).start >= chapterCodepointOffsets(getParagraphs()).length) {
      advanceText().then(moved => {
        if (destroyed || epoch !== speechEpoch || mode !== 'speech' || paused) return;
        if (moved) speak(textItem()); else { mode = 'manual'; setButtons(); }
      });
      return;
    }
    const start = from === null ? panelRange(item, getParagraphs()).start : from; const result = speechFor(item, start); speechBase = start;
    speechUtterance = new SpeechSynthesisUtterance(result.text); speechUtterance.rate = speed;
    speechUtterance.onboundary = event => {
      if (epoch !== speechEpoch) return;
      const delta = Array.from(result.text.slice(0, Math.max(0, Number(event.charIndex) || 0))).length;
      const pos = positionForCodepoint(speechBase + delta, getParagraphs()); updateActive(item.chapter, pos.para, pos.offset);
      if (typeof onTextProgress === 'function') {
        internalNavigation++;
        Promise.resolve(onTextProgress({ chapter: item.chapter, para: pos.para, offset: pos.offset })).finally(() => {
          internalNavigation--;
          if (epoch === speechEpoch) updateActive(item.chapter, pos.para, pos.offset);
        });
      }
    };
    speechUtterance.onerror = event => {
      if (epoch !== speechEpoch) return;
      speechUtterance = null;
      paused = false;
      mode = 'manual';
      setButtons();
      if (event?.error !== 'canceled' && event?.error !== 'interrupted') toast('语音朗读失败，请检查浏览器语音设置');
    };
    speechUtterance.onend = async () => {
      if (epoch !== speechEpoch) return;
      speechUtterance = null;
      const pos = positionForCodepoint(result.range.end, getParagraphs());
      updateActive(item.chapter, pos.para, pos.offset);
      if (typeof onTextProgress === 'function') {
        internalNavigation++;
        try { await onTextProgress({ chapter: item.chapter, para: pos.para, offset: pos.offset }); }
        finally { internalNavigation--; }
      }
      if (epoch !== speechEpoch || destroyed) return;
      updateActive(item.chapter, pos.para, pos.offset);
      const hasMoreText = mode === 'speech' && !paused ? await advanceText() : false;
      if (epoch !== speechEpoch || destroyed) return;
      if (mode === 'speech' && !paused && hasMoreText) speak(textItem());
      else if (!paused) { mode = 'manual'; setButtons(); }
    };
    speechSynthesis.speak(speechUtterance); setButtons();
  }
  async function refresh(force = false) {
    const now = Date.now(); if (refreshPromise || (!force && now - lastRefresh < 1200)) return refreshPromise;
    lastRefresh = now;
    refreshPromise = apiGet('/api/books/' + encodeURIComponent(bookId) + '/manga/reading-map').then(data => {
      if (destroyed) return all;
      all = normalizeReadingMap(data); renderChapter(state().chapter);
      const pos = cursor?.chapter === Number(state().chapter) ? positionForCodepoint(cursor.value, getParagraphs()) : state();
      updateActive(state().chapter, pos.para, pos.offset); return all;
    }).catch(() => all).finally(() => { refreshPromise = null; });
    return refreshPromise;
  }

  manual.addEventListener('click', () => { stopPlayback(); paused = false; mode = 'manual'; scrollMode(false); setButtons(); });
  auto.addEventListener('click', () => {
    if (mode === 'auto') { if (paused) { paused = false; setButtons(); startAuto(); } return; }
    stopPlayback(); paused = false; mode = 'auto'; setButtons(); startAuto();
  });
  speech.addEventListener('click', async () => {
    if (mode === 'speech' && speechUtterance) return;
    stopPlayback(); paused = false; mode = 'speech'; setButtons();
    const token = playEpoch;
    await scrollMode(true);
    if (!destroyed && token === playEpoch && mode === 'speech' && !paused) speak(textItem());
  });
  pause.addEventListener('click', () => {
    if (mode === 'speech') {
      if (speechUtterance && typeof speechSynthesis !== 'undefined') {
        if (paused || speechSynthesis.paused) { speechSynthesis.resume(); paused = false; }
        else { speechSynthesis.pause(); paused = true; }
      } else { paused = !paused; if (!paused) speak(textItem()); }
    } else if (mode === 'auto') { paused = !paused; playEpoch++; stopTimer(); if (!paused) startAuto(); }
    setButtons();
  });
  rate.addEventListener('change', () => {
    speed = Number(rate.value) || 1;
    if (mode === 'speech' && speechUtterance) {
      const wasPaused = paused; const at = cursor?.chapter === Number(state().chapter) ? cursor.value : null;
      speak(textItem(), at); if (wasPaused && typeof speechSynthesis !== 'undefined') speechSynthesis.pause();
    }
    if (mode === 'auto' && !paused) { playEpoch++; stopTimer(); startAuto(); }
    setButtons();
  });

  const onManualInput = () => {
    if (mode === 'manual') return;
    stopPlayback(); mode = 'manual'; paused = false; setButtons();
  };
  const textViewport = pager.viewport || main.querySelector?.('.reader-viewport');
  for (const event of ['pointerdown', 'wheel', 'touchstart', 'keydown']) textViewport?.addEventListener(event, onManualInput, { passive: true });
  renderChapter(state().chapter); updateActive(state().chapter, state().para, state().offset, false); setButtons(); refresh();
  return {
    update(info) {
      if (!info || destroyed) return;
      const nextCursor = cursorFor(info.chapter, info.para, info.offset);
      const moved = !cursor || nextCursor.chapter !== cursor.chapter || Math.abs(nextCursor.value - cursor.value) > 1;
      if (mode === 'speech' && speechUtterance && !internalNavigation && moved) {
        stopSpeech(); updateActive(info.chapter, info.para, info.offset);
        if (!paused) speak(textItem());
      } else updateActive(info.chapter, info.para, info.offset);
    },
    refresh: () => refresh(true),
    destroy() {
      destroyed = true; playEpoch++; stopTimer(); stopSpeech();
      for (const event of ['pointerdown', 'wheel', 'touchstart', 'keydown']) textViewport?.removeEventListener(event, onManualInput);
      controls.remove(); root.remove(); main.classList.remove('with-comics');
      footNode?.classList.remove('has-reading-sync');
    },
    mode: () => mode,
  };
}
