import { el, apiPatch, fmtTime, toast } from '../util/core.js';

export function readingState(book) {
  // CBZ 的默认百分比代表第一页的位置，不代表用户已经打开阅读。
  if (!book.has_progress) return 'unread';
  return Number(book.percent) >= .999 ? 'finished' : 'reading';
}

export function readingLabel(book) {
  const state = readingState(book);
  if (state === 'unread') return '尚未开始';
  const pct = Math.max(0, Math.min(100, Number(book.percent) * 100 || 0));
  const percent = pct > 0 && pct < .1 ? '<0.1%' : pct.toFixed(1) + '%';
  const pos = '第 ' + (Number(book.last_chapter || 0) + 1) + (book.format === 'cbz' ? ' 页' : ' 章');
  return (state === 'finished' ? '已读完' : '读到 ' + pos) + ' · ' + percent;
}

export function coverHue(book) {
  let hash = 2166136261;
  for (const ch of String(book.title || '书')) hash = Math.imul(hash ^ ch.codePointAt(0), 16777619);
  // 同名不同格式固定错开色相；标题变化后配色仍可复现。
  return ((hash >>> 0) % 360 + ({ txt: 0, epub: 115, cbz: 235 }[book.format] || 0)) % 360;
}

export function shelfPreferences() {
  try { return JSON.parse(localStorage.getItem('velmira.shelf') || '{}') || {}; }
  catch { return {}; }
}

export function saveShelfPreferences(value) {
  try { localStorage.setItem('velmira.shelf', JSON.stringify(value)); } catch { /* storage unavailable */ }
}

export function openBookDetails(book, meta, onChanged) {
  const prior = document.activeElement;
  const dialog = el('dialog', 'library-dialog');
  dialog.setAttribute('aria-label', '书籍详情');
  const head = el('div', 'drawer-head');
  head.append(el('h2', 'drawer-title', '书籍详情'));
  const close = el('button', 'btn btn-icon', '×');
  close.type = 'button'; close.setAttribute('aria-label', '关闭书籍详情');
  close.addEventListener('click', () => dialog.close());
  head.append(close);
  const body = el('div', 'drawer-body book-details');
  const cover = el('div', 'detail-cover cover-' + book.format, book.title);
  cover.style.setProperty('--book-hue', coverHue(book));
  cover.setAttribute('aria-hidden', 'true');
  body.append(cover, el('h3', 'detail-title', book.title), el('p', 'detail-meta', meta),
    el('p', 'detail-progress', readingLabel(book)));
  const facts = el('dl', 'detail-facts');
  for (const [label, value] of [['格式', String(book.format).toUpperCase()], ['导入时间', fmtTime(book.created_at)], ['最近阅读', fmtTime(book.last_read_at)]]) {
    facts.append(el('dt', null, label), el('dd', null, value));
  }
  const form = el('form', 'rename-form');
  const label = el('label', null, '书名');
  const input = el('input', 'shelf-input');
  input.value = book.title; input.maxLength = 200; input.required = true;
  input.setAttribute('aria-label', '书名'); label.append(input);
  const save = el('button', 'btn', '保存书名'); save.type = 'submit';
  const message = el('p', 'management-message'); message.setAttribute('role', 'status');
  form.append(label, save);
  const archived = !!book.archived_at;
  const archive = el('button', 'btn ' + (archived ? 'btn-primary' : 'btn-danger'), archived ? '恢复到书架' : '移出书架');
  archive.type = 'button';
  const note = el('p', 'detail-help', '移出后可在“状态 → 已移出”中恢复，正文、进度、批注和 AI 数据都会保留。');
  let busy = false;
  async function update(patch) {
    if (busy) return;
    busy = true; save.disabled = true; archive.disabled = true; message.textContent = '正在保存…';
    try {
      await apiPatch('/api/books/' + encodeURIComponent(book.id), patch);
      await onChanged();
      dialog.close();
      toast('title' in patch ? '书名已更新' : (patch.archived ? '已移出书架，可随时恢复' : '已恢复到书架'));
    } catch (err) { message.textContent = '保存失败：' + err.message; }
    finally { busy = false; save.disabled = false; archive.disabled = false; }
  }
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    if (!input.value.trim()) { message.textContent = '请输入书名'; input.focus(); return; }
    update({ title: input.value.trim() });
  });
  archive.addEventListener('click', () => update({ archived: !archived }));
  const source = el('a', 'btn btn-ghost', '下载原文件');
  source.href = '/api/books/' + encodeURIComponent(book.id) + '/source';
  source.download = book.title + '.' + book.format;
  body.append(facts, source, form, message, archive, note);
  dialog.append(head, body);
  dialog.addEventListener('click', (e) => { if (e.target === dialog) { const r = dialog.getBoundingClientRect(); if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) dialog.close(); } });
  dialog.addEventListener('close', () => { dialog.remove(); if (prior?.isConnected) prior.focus(); }, { once: true });
  document.body.append(dialog); dialog.showModal(); close.focus();
}
