import { el, apiGet, apiPost, toast } from '../util/core.js';

const SIZE_OPTIONS = [['portrait', '竖版 1024×1536'], ['tall', '长竖版 832×1216'], ['square', '方形 1024×1024']];
const PANEL_OPTIONS = [[4, '4 格 · 2 上 + 1 中 + 1 下'], [3, '3 格 · 1 上 + 2 下'], [2, '2 格 · 上下'], [5, '5 格 · 2 + 2 + 1'], [6, '6 格 · 3 行 2 列']];
const STATES = { queued: '排队中', running: '生成中', done: '已生成', error: '生成失败' };

export async function mountSinglePage({ area, bookId, signal }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/manga/single-page';
  area.replaceChildren(el('p', 'manga-note', '正在读取章节…'));
  let toc;
  try { toc = await apiGet('/api/books/' + encodeURIComponent(bookId) + '/toc', signal); }
  catch (err) { area.replaceChildren(el('p', 'manga-note', '章节加载失败：' + err.message)); return; }
  if (signal.aborted) return;
  const chapters = toc.chapters || [];
  area.replaceChildren();
  const note = el('p', 'manga-note', '文本模型先规划 2–6 个连续分格，图像模型再一次生成整页：所有分格和黑色边框画在同一张图里，不生成任何对白、旁白或文字。适合快速得到一张无字漫画草稿；分格位置由图像模型理解，不保证像素级对齐。');
  const controls = el('div', 'manga-actions');
  const chapter = el('select'); chapter.setAttribute('aria-label', '无字漫画章节');
  const urlChapter = Number(new URL(location.href).searchParams.get('ch'));
  for (const item of chapters) {
    const option = el('option', null, '第 ' + (item.idx + 1) + ' 章 · ' + item.title); option.value = String(item.idx); chapter.appendChild(option);
  }
  if (chapters.some(item => item.idx === urlChapter)) chapter.value = String(urlChapter);
  const chapterWrap = el('label', 'manga-field'); chapterWrap.append(el('span', null, '章节'), chapter);
  const panels = el('select'); panels.setAttribute('aria-label', '无字漫画分格数');
  for (const [value, label] of PANEL_OPTIONS) { const option = el('option', null, label); option.value = String(value); panels.appendChild(option); }
  const panelsWrap = el('label', 'manga-field'); panelsWrap.append(el('span', null, '分格数'), panels);
  const size = el('select'); size.setAttribute('aria-label', '无字漫画页面尺寸');
  for (const [value, label] of SIZE_OPTIONS) { const option = el('option', null, label); option.value = value; size.appendChild(option); }
  const sizeWrap = el('label', 'manga-field'); sizeWrap.append(el('span', null, '页面尺寸'), size);
  const generate = el('button', 'btn btn-primary', '一次生成无字漫画页'); generate.type = 'button';
  controls.append(chapterWrap, panelsWrap, sizeWrap, generate);
  const status = el('p', 'manga-note'); status.setAttribute('role', 'status');
  const preview = el('div', 'manga-plan'); preview.hidden = true;
  const history = el('div', 'manga-jobs');
  const historyHead = el('h3', 'ai-section-title', '无字漫画记录');
  area.append(note, controls, status, preview, historyHead, history);
  let current = null, timer = 0, busy = false;

  function draw(page) {
    current = page;
    if (!page) { preview.hidden = true; status.textContent = chapters.length ? '选择章节和分格数后生成。' : '这本书没有文字章节。'; generate.disabled = !chapters.length; return; }
    generate.disabled = busy || ['queued', 'running'].includes(page.status);
    preview.hidden = false;
    preview.replaceChildren();
    const head = el('div', 'manga-actions');
    head.append(el('h4', null, '第 ' + (page.chapter + 1) + ' 章 · ' + page.panels + ' 格 · ' + (STATES[page.status] || page.status)));
    preview.appendChild(head);
    status.textContent = (STATES[page.status] || page.status) + (page.error ? '：' + page.error : '');
    if (page.url) {
      const link = el('a', 'manga-preview'); link.href = page.url; link.target = '_blank'; link.rel = 'noopener';
      const image = el('img'); image.src = page.url; image.alt = '无字漫画第 ' + (page.chapter + 1) + ' 章'; image.loading = 'eager';
      link.appendChild(image); preview.appendChild(link);
      preview.appendChild(el('p', 'manga-note', '右键或点开大图保存；图片不含任何文字，可直接作为上字底稿。'));
    }
    const spec = page.spec || {};
    if (spec.panels?.length) {
      const details = el('details', 'manga-editor'); details.append(el('summary', null, '查看分格规划'));
      spec.panels.forEach((panel, index) => {
        details.appendChild(el('p', 'manga-note', '第 ' + (index + 1) + ' 格 · ' + panel.shot + ' · ' + panel.action + (panel.environment ? ' · ' + panel.environment : '')));
      });
      preview.appendChild(details);
    }
  }
  async function refreshList() {
    const data = await apiGet(base, signal);
    if (signal.aborted) return;
    history.replaceChildren();
    if (!data.pages?.length) { history.appendChild(el('p', 'manga-note', '还没有生成记录。')); return; }
    for (const page of data.pages) {
      const item = el('article', 'manga-job');
      item.append(el('h4', null, '第 ' + (page.chapter + 1) + ' 章 · ' + page.panels + ' 格 · ' + (STATES[page.status] || page.status)));
      if (page.url) {
        const link = el('a', 'manga-preview'); link.href = page.url; link.target = '_blank'; link.rel = 'noopener';
        const image = el('img'); image.src = page.url; image.alt = '无字漫画页'; image.loading = 'lazy'; link.appendChild(image); item.appendChild(link);
      } else {
        item.appendChild(el('p', 'manga-note', page.error || (page.status === 'queued' || page.status === 'running' ? '正在生成，请稍候…' : '没有图片')));
      }
      if (page.status === 'done') {
        const actions = el('div', 'manga-actions');
        const use = el('button', 'btn', '查看'); use.type = 'button'; use.addEventListener('click', () => { draw(page); preview.scrollIntoView({ block: 'start', behavior: 'smooth' }); });
        actions.appendChild(use); item.appendChild(actions);
      }
      history.appendChild(item);
    }
    const running = data.pages.find(page => page.status === 'queued' || page.status === 'running');
    if (running) { draw(running); schedule(); }
    else if (current) {
      const latest = data.pages.find(page => page.id === current.id) || data.pages[0];
      if (latest) draw(latest);
    }
  }
  function schedule() {
    clearTimeout(timer);
    if (signal.aborted) return;
    timer = setTimeout(async () => {
      try { await refreshList(); } catch (err) { if (!signal.aborted) status.textContent = '读取生成状态失败：' + err.message; }
    }, 2200);
  }
  generate.addEventListener('click', async () => {
    if (busy || !chapters.length) return;
    busy = true; generate.disabled = true; preview.hidden = true;
    status.textContent = '正在规划分格并请求图像模型…';
    try {
      const data = await apiPost(base, { chapter: Number(chapter.value), panels: Number(panels.value), size: size.value }, signal);
      if (signal.aborted) return;
      draw(data.page);
      busy = false;
      await refreshList();
    } catch (err) {
      busy = false;
      if (!signal.aborted) { status.textContent = '生成失败：' + err.message; generate.disabled = false; toast(err.message, 5000); }
    }
  });
  signal.addEventListener('abort', () => clearTimeout(timer), { once: true });
  draw(null);
  try { await refreshList(); }
  catch (err) { if (!signal.aborted) status.textContent = '读取生成记录失败：' + err.message; }
}
