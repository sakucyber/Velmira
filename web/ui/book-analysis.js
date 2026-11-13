import { el, apiGet, apiPost, toast } from '../util/core.js';

export function mountBookAnalysis({ area, bookId, signal }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/ai/analysis';
  const status = el('p', 'manga-note', '正在读取整书分析进度…');
  const progress = el('progress'); progress.setAttribute('aria-label', '整书阅读进度'); progress.style.width = '100%';
  const actions = el('div', 'manga-actions');
  const start = el('button', 'btn btn-primary', '开始 / 继续整书分析');
  const pause = el('button', 'btn', '暂停分析');
  const list = el('div'); let timer, signature = '';
  area.append(el('p', 'manga-note', '自动阅读全书，逐章保存大意、人物卡和可绘制场景。长章节会分段处理；关闭软件后可继续。外貌有变化时保留原文证据，冻结的身份不会被覆盖。'), status, progress, actions, list);
  actions.append(start, pause);
  const states = { idle: '尚未开始', queued: '排队中', running: '阅读中', paused: '已暂停', error: '已中断', done: '整书分析完成' };
  function draw(data) {
    if (signal.aborted) return;
    const a = data.analysis;
    status.textContent = (states[a.status] || a.status) + ' · 已完成 ' + a.completed + ' / ' + a.total + ' 章'
      + (a.status === 'running' && a.current_chapter != null ? ' · 第 ' + (a.current_chapter + 1) + ' 章，分段 ' + a.current_part + ' / ' + a.total_parts : '')
      + (a.error ? ' · ' + a.error : '');
    progress.max = Math.max(1, a.total); progress.value = a.completed;
    start.disabled = ['queued', 'running'].includes(a.status);
    pause.disabled = !['queued', 'running'].includes(a.status);
    const next = JSON.stringify(data.chapters);
    if (next === signature) return;
    signature = next;
    const opened = new Set([...list.querySelectorAll('details[open]')].map(n => n.dataset.chapter));
    list.replaceChildren();
    for (const ch of data.chapters) {
      const detail = el('details', 'manga-editor'); detail.dataset.chapter = String(ch.chapter);
      detail.open = opened.has(detail.dataset.chapter);
      detail.append(el('summary', null, '第 ' + (ch.chapter + 1) + ' 章 · ' + ch.title), el('p', 'manga-note', ch.summary));
      detail.append(el('p', 'manga-note', ch.character_ids.length + ' 位人物 · ' + ch.parts + ' 段正文 · ' + ch.entities.join('、')));
      list.appendChild(detail);
    }
  }
  async function action(path) {
    start.disabled = pause.disabled = true;
    try { draw(await apiPost(base + path, {})); }
    catch (err) { if (!signal.aborted) { toast(err.message, 5000); start.disabled = false; } }
  }
  start.addEventListener('click', () => action(''));
  pause.addEventListener('click', () => action('/pause'));
  async function poll() {
    try { draw(await apiGet(base, signal)); }
    catch (err) { if (!signal.aborted) status.textContent = '读取进度失败：' + err.message; }
    if (!signal.aborted) timer = setTimeout(poll, 2500);
  }
  signal.addEventListener('abort', () => clearTimeout(timer), { once: true });
  poll();
}
