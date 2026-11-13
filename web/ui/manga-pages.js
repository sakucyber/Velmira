import { el, apiGet, apiPost, apiPatch, apiPut, apiDelete, toast } from '../util/core.js';

const LABELS = { auto: '自动选择', '1x1': '单格', '2x1': '左右双格', '2x2': '四格', '2x3': '两列三行', '3x2': '三列两行', '1+2': '上大下双', '2+1': '上双下大' };

export async function mountMangaPages({ area, bookId, signal }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/manga';
  let data, panels = [], editions = [], editing = null, busy = false, slots = [];
  const selected = new Set();
  const root = el('div', 'page-studio'); area.replaceChildren(root);
  const feedback = el('p', 'manga-note'); feedback.setAttribute('role', 'status');
  root.append(el('p', 'manga-note', '选择已通过检查或人工确认的分镜，拼成漫画页，再添加可编辑的旁白和对白。导出的 CBZ 会同时加入书架。'), feedback);
  function button(parent, label, action, cls = '') {
    const b = el('button', 'btn ' + cls, label); b.type = 'button'; parent.appendChild(b);
    b.addEventListener('click', async () => {
      if (busy || signal.aborted) return;
      busy = true; b.disabled = true; feedback.textContent = '正在处理…';
      try { await action(); if (!signal.aborted && feedback.textContent === '正在处理…') feedback.textContent = '已完成'; }
      catch (err) { if (!signal.aborted) { feedback.textContent = err.message; toast(err.message, 5000); } }
      finally { busy = false; b.disabled = false; }
    }); return b;
  }
  function field(parent, label, value, type = 'text') {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, label));
    const input = el(type === 'textarea' ? 'textarea' : 'input'); if (type !== 'textarea') input.type = type;
    input.value = value ?? ''; input.setAttribute('aria-label', label); wrap.appendChild(input); parent.appendChild(wrap); return input;
  }
  function select(parent, label, choices, value) {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, label));
    const input = el('select'); input.setAttribute('aria-label', label);
    for (const [key, title] of choices) { const option = el('option', null, title); option.value = key; input.appendChild(option); }
    input.value = value; wrap.appendChild(input); parent.appendChild(wrap); return input;
  }
  function preview(parent, url, label, cls) {
    const image = el('img', cls); if (url) image.src = url; image.alt = label; image.loading = 'lazy'; parent.appendChild(image); return image;
  }
  async function load() {
    const [p, candidates, exports] = await Promise.all([apiGet(base + '/pages', signal), apiGet(base + '/page-panels', signal), apiGet(base + '/exports', signal)]);
    data = p; panels = candidates.panels; editions = exports.exports;
  }
  await load(); if (signal.aborted) return;
  const editor = el('section', 'manga-editor page-editor'); root.appendChild(editor);
  const heading = el('h3', 'ai-section-title', '新建漫画页'); editor.appendChild(heading);
  const title = field(editor, '页标题（可留空）', ''); title.maxLength = 100;
  const controls = el('div', 'page-controls'); editor.appendChild(controls);
  const layout = select(controls, '拼页模板', Object.entries(LABELS), 'auto');
  const font = select(controls, '文字字体', [['sans', '黑体'], ['serif', '宋体']], 'sans');
  const size = field(controls, '文字字号', 32, 'number'); size.min = '18'; size.max = '72';
  const width = field(controls, '页面宽度', 1200, 'number'); width.min = '640'; width.max = '2400';
  const height = field(controls, '页面高度', 1800, 'number'); height.min = '960'; height.max = '3600';
  const gutter = field(controls, '格间白边', 8, 'number'); gutter.min = '4'; gutter.max = '48';
  const slotArea = el('div'); editor.appendChild(slotArea);
  function common() { return { title: title.value, layout: layout.value, font: font.value, font_size: Number(size.value), width: Number(width.value), height: Number(height.value), gutter: Number(gutter.value) }; }
  function slotValues() { return slots.map(s => ({ job_id: s.pick.value, caption: s.caption.value, dialogue: s.dialogue.value, speaker: s.speaker.value, bubble: s.bubble.value, align: s.align.value, fit: s.fit.value })); }
  function showSlots(values) {
    slots = []; slotArea.replaceChildren();
    values.forEach((value, idx) => {
      const card = el('div', 'page-slot'); card.dataset.slot = String(idx); slotArea.appendChild(card);
      card.appendChild(el('h4', null, '第 ' + (idx + 1) + ' 格'));
      const eligible = panels.filter(p => ['done', 'approved'].includes(p.status) || p.id === value.job_id);
      const pick = select(card, '第 ' + (idx + 1) + ' 格分镜', eligible.map(p => [p.id, '第 ' + (p.chapter + 1) + ' 章 · ' + p.summary.slice(0, 90)]), value.job_id || eligible[0]?.id || '');
      const image = preview(card, eligible.find(p => p.id === pick.value)?.url || '', '第 ' + (idx + 1) + ' 格原图', 'page-slot-image'); image.hidden = !pick.value;
      pick.addEventListener('change', () => { const url = panels.find(p => p.id === pick.value)?.url; if (url) image.src = url; else image.removeAttribute('src'); image.hidden = !url; });
      const caption = field(card, '第 ' + (idx + 1) + ' 格旁白', value.caption, 'textarea'); caption.maxLength = 300;
      button(card, '填入场景旁白', () => {
        const text = panels.find(p => p.id === pick.value)?.summary || '';
        if (text.length > 300) throw new Error('场景摘要超过 300 字，请自行精简后填入旁白');
        caption.value = text;
      });
      const dialogue = field(card, '第 ' + (idx + 1) + ' 格对白', value.dialogue, 'textarea'); dialogue.maxLength = 400;
      const speaker = field(card, '第 ' + (idx + 1) + ' 格说话人（可留空）', value.speaker); speaker.maxLength = 40;
      const row = el('div', 'page-controls'); card.appendChild(row);
      const bubble = select(row, '第 ' + (idx + 1) + ' 格气泡', [['rounded', '圆角气泡'], ['ellipse', '椭圆气泡']], value.bubble || 'rounded');
      const align = select(row, '第 ' + (idx + 1) + ' 格气泡朝向', [['left', '朝左'], ['right', '朝右']], value.align || 'left');
      const fit = select(row, '第 ' + (idx + 1) + ' 格图像适配', [['contain', '完整显示'], ['cover', '裁切填满']], value.fit || 'contain');
      slots.push({ pick, caption, dialogue, speaker, bubble, align, fit });
      const actions = el('div', 'manga-actions'); card.appendChild(actions);
      if (idx > 0) button(actions, '本格前移', () => { const v = slotValues(); [v[idx - 1], v[idx]] = [v[idx], v[idx - 1]]; showSlots(v); });
      button(actions, '移除本格', () => { showSlots(slotValues().filter((_, i) => i !== idx)); });
    });
  }
  showSlots([{ job_id: panels.find(p => ['done', 'approved'].includes(p.status))?.id || '' }]);
  const editActions = el('div', 'manga-actions'); editor.appendChild(editActions);
  button(editActions, '添加一格', () => { if (slots.length >= 6) throw new Error('每页最多 6 格'); showSlots([...slotValues(), {}]); });
  const save = button(editActions, '保存并预览漫画页', async () => {
    const spec = { ...common(), panels: slotValues() };
    const result = editing ? await apiPatch(base + '/pages/' + editing.id, { revision: editing.revision, spec }) : await apiPost(base + '/pages', spec);
    editing = result.page; heading.textContent = '编辑漫画页'; await refresh();
    gallery.querySelector('[data-page="' + editing.id + '"]')?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }, 'btn-primary');
  button(editActions, '另建新页', () => { editing = null; heading.textContent = '新建漫画页'; title.value = ''; });

  const candidates = el('details', 'manga-editor'); candidates.appendChild(el('summary', null, '选择分镜 · 自动拼页 / 人工复核'));
  root.appendChild(candidates);
  const candidateList = el('div', 'page-candidates'); candidates.appendChild(candidateList);
  button(candidates, '将选中分镜自动拼页', async () => {
    if (!selected.size) throw new Error('请先勾选分镜');
    await apiPost(base + '/pages/auto', { job_ids: [...selected], settings: common() }); selected.clear(); await refresh();
  });
  candidates.appendChild(el('p', 'manga-note', '沿用上方页面设置，按章节分组、按勾选顺序拼页；自动模板每页最多四格。文字可在生成后逐页编辑。'));
  const gallery = el('div', 'page-gallery'); root.append(el('h3', 'ai-section-title', '漫画页'), gallery);
  const exportsArea = el('div', 'page-exports'); root.appendChild(exportsArea);
  button(root, '导出 CBZ 并加入书架', async () => {
    const result = await apiPost(base + '/exports', {}); await refresh();
    feedback.textContent = '已导出 ' + result.export.page_count + ' 页，可下载或在阅读器打开';
  }, 'btn-primary');
  button(root, '刷新分镜与漫画页', refresh);
  const trash = el('details', 'manga-editor'); trash.appendChild(el('summary', null, '已移除漫画页'));
  const trashList = el('div'); trash.appendChild(trashList); root.appendChild(trash);
  async function refreshTrash() {
    const removed = await apiGet(base + '/pages?deleted=true', signal); trashList.replaceChildren();
    for (const page of removed.pages) {
      const line = el('div', 'manga-actions'); line.appendChild(el('span', null, page.spec.title || '无标题漫画页'));
      button(line, '恢复此页', async () => { await apiPost(base + '/pages/' + page.id + '/restore', { revision: page.revision }); await refresh(); await refreshTrash(); }); trashList.appendChild(line);
    }
    if (!removed.pages.length) trashList.appendChild(el('p', 'manga-note', '没有已移除的页'));
  }
  trash.addEventListener('toggle', () => { if (trash.open) refreshTrash().catch(err => toast(err.message)); });
  async function refresh() {
    const draft = slotValues();
    await load(); if (signal.aborted) return;
    for (const id of selected) if (!panels.some(p => p.id === id && ['done', 'approved'].includes(p.status))) selected.delete(id);
    showSlots(draft); renderLists();
  }
  function renderLists() {
    candidateList.replaceChildren(); gallery.replaceChildren(); exportsArea.replaceChildren();
    for (const panel of panels) {
      const card = el('article', 'page-candidate'); candidateList.appendChild(card);
      preview(card, panel.url, '分镜候选', 'page-candidate-image');
      const label = el('label', 'page-select'); const input = el('input'); input.type = 'checkbox'; input.checked = selected.has(panel.id);
      input.disabled = !['done', 'approved'].includes(panel.status); input.setAttribute('aria-label', '选择分镜 ' + panel.id);
      input.addEventListener('change', () => { input.checked ? selected.add(panel.id) : selected.delete(panel.id); });
      label.append(input, el('span', null, '第 ' + (panel.chapter + 1) + ' 章 · ' + panel.summary)); card.appendChild(label);
      if (panel.status === 'needs_review') button(card, '人工确认这张分镜', async () => { await apiPost(base + '/jobs/' + panel.id + '/approve', {}); await refresh(); });
      else if (!['done', 'approved'].includes(panel.status)) card.appendChild(el('p', 'manga-note', panel.error || '此分镜暂不可用于拼页'));
    }
    if (!panels.length) candidateList.appendChild(el('p', 'manga-note', '请先在分镜页生成图片。'));
    data.pages.forEach((page, idx) => {
      const card = el('article', 'page-card'); card.dataset.page = page.id; gallery.appendChild(card);
      card.appendChild(el('h4', null, '第 ' + (idx + 1) + ' 页 · ' + (page.spec.title || LABELS[page.spec.layout])));
      const link = el('a'); link.href = page.url; link.target = '_blank'; link.rel = 'noopener';
      preview(link, page.url, '漫画第 ' + (idx + 1) + ' 页预览', 'page-preview'); card.appendChild(link);
      if (page.error) card.appendChild(el('p', 'manga-note', '需要更新：' + page.error));
      const actions = el('div', 'manga-actions'); card.appendChild(actions);
      button(actions, '编辑此页', () => {
        editing = page; const spec = page.spec; heading.textContent = '编辑第 ' + (idx + 1) + ' 页';
        title.value = spec.title; layout.value = spec.layout; font.value = spec.font; size.value = spec.font_size;
        width.value = spec.width; height.value = spec.height; gutter.value = spec.gutter; showSlots(spec.panels); editor.scrollIntoView({ block: 'start', behavior: 'smooth' });
      });
      for (const [step, label] of [[-1, '本页前移'], [1, '本页后移']]) {
        if (idx + step < 0 || idx + step >= data.pages.length) continue;
        button(actions, label, async () => {
          const order = data.pages.map(p => p.id); [order[idx], order[idx + step]] = [order[idx + step], order[idx]];
          await apiPut(base + '/pages/order', { order, revisions: Object.fromEntries(data.pages.map(p => [p.id, p.revision])) }); await refresh();
          if (editing) editing = data.pages.find(p => p.id === editing.id) || null;
        });
      }
      button(actions, '移除此页', async () => { await apiDelete(base + '/pages/' + page.id + '?revision=' + page.revision); if (editing?.id === page.id) { editing = null; heading.textContent = '新建漫画页'; } await refresh(); });
    });
    if (!data.pages.length) gallery.appendChild(el('p', 'manga-note', '尚无漫画页。可编辑单页，或选择分镜批量拼页。'));
    for (const edition of editions) {
      const row = el('div', 'manga-actions'); row.appendChild(el('span', null, edition.page_count + ' 页 · 已导出版本'));
      const read = el('a', 'btn', '阅读漫画'); read.href = edition.read_url;
      const download = el('a', 'btn', '下载 CBZ'); download.href = edition.download_url; download.download = '';
      row.append(read, download); exportsArea.appendChild(row);
    }
    save.disabled = !panels.some(p => ['done', 'approved'].includes(p.status));
  }
  renderLists();
}
