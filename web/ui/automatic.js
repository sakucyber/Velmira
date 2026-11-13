import { el, apiGet, apiPatch, toast } from '../util/core.js';

const names = { idle: '尚未开启', queued: '准备生成', analyzing: '整书分析', planning: '编排连续分镜',
  reference: '人物参考图', directing: '情节总结与提示词', generating: '逐格生成', paused: '已暂停', error: '已中断', done: '已完成' };

export function mountAutomatic({ area, bookId, signal, chapter, onSettings, onChange }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/manga/auto';
  const note = el('p', 'manga-note', '从开启时的阅读章节开始，按顺序生成后续章节。AI 先阅读全书，再编排连续分镜，复用人物参考图与画风，自动加入旁白并拼页。');
  const control = el('button', 'btn automatic-switch', '正在读取…'); control.type = 'button'; control.disabled = true;
  control.setAttribute('role', 'switch'); control.setAttribute('aria-label', '自动生成连贯漫画'); control.setAttribute('aria-checked', 'false');
  const status = el('p', 'manga-note'); status.setAttribute('role', 'status');
  const progress = el('progress'); progress.setAttribute('aria-label', '自动漫画章节进度'); progress.style.width = '100%';
  const settings = el('button', 'btn', '模型与画风设置'); settings.type = 'button'; settings.addEventListener('click', onSettings);
  const actions = el('div', 'manga-actions'); actions.append(control, settings);
  const gallery = el('div', 'automatic-gallery');
  const history = el('select'); history.setAttribute('aria-label', '选择漫画生成记录');
  const historyLabel = el('label', 'manga-field'); historyLabel.append(el('span', null, '漫画生成记录'), history);
  const latest = el('option', null, '本次生成'); latest.value = ''; history.appendChild(latest);
  const more = el('button', 'btn', '加载更多漫画页'); more.type = 'button'; more.hidden = true;
  area.append(note, actions, status, progress,
    el('p', 'manga-note', '开启后会持续调用已配置的文本和图像 API。关闭开关暂停后续任务；正在处理的一张图会保存。关闭软件后进度保留，下次启动继续。'),
    el('p', 'manga-note', '以下为自动漫画草稿。参考图由程序选用；待复核图片会明确标记，可在漫画生成记录中检查。'), historyLabel, gallery, more);
  let current, timer, busy = false, rendered = 0, generation = '', total = -1, loading = false;
  function draw(value) {
    current = value; onChange?.(value);
    control.disabled = busy; control.setAttribute('aria-checked', String(value.enabled));
    control.textContent = value.enabled ? '自动漫画 · 已开启' : value.status === 'idle' || value.status === 'done' ? '从当前章开启' : '继续自动漫画';
    status.textContent = (names[value.status] || value.status) + ' · ' + value.message
      + (value.start_chapter != null ? ' · 从第 ' + (value.start_chapter + 1) + ' 章开始' : '')
      + ' · 已完成 ' + value.completed_chapters + '/' + value.total_chapters + ' 章 · ' + value.pages + ' 页';
    if (value.analysis) status.textContent += ' · 全书分析 ' + value.analysis.completed + '/' + value.analysis.total + ' 章';
    progress.max = Math.max(1, value.total_chapters); progress.value = value.completed_chapters;
  }
  async function loadPages(reset = false) {
    if (loading || signal.aborted) return;
    loading = true; more.disabled = true;
    try {
      const offset = reset ? 0 : rendered;
      const data = await apiGet(base + '/pages?offset=' + offset + '&limit=12' + (history.value ? '&run_id=' + encodeURIComponent(history.value) : ''), signal);
      if (signal.aborted) return;
      if (reset) { gallery.replaceChildren(); rendered = 0; }
      gallery.querySelector('.automatic-empty')?.remove();
      for (const page of data.pages) {
        const card = el('article', 'automatic-page');
        card.append(el('h3', 'ai-section-title', '第 ' + (page.chapter + 1) + ' 章 · 第 ' + (page.ordinal + 1) + ' 页 · ' + page.title),
          el('p', 'manga-note', page.error ? '需要处理：' + page.error : '自动草稿' + (page.pending_review ? ' · ' + page.pending_review + ' 格待复核' : '')));
        const image = el('img'); image.src = page.url; image.alt = page.title + '，漫画第 ' + (page.ordinal + 1) + ' 页'; image.loading = 'lazy';
        card.appendChild(image); gallery.appendChild(card);
      }
      rendered += data.pages.length; total = data.total; more.hidden = !data.has_more;
      if (!rendered) gallery.appendChild(el('p', 'manga-note automatic-empty', '第一张漫画页完成后会自动显示在这里。'));
    } catch (err) { if (!signal.aborted) toast('漫画页加载失败：' + err.message); }
    finally { loading = false; more.disabled = false; }
  }
  control.addEventListener('click', async () => {
    if (!current || busy) return;
    busy = true; control.disabled = true;
    try { const data = await apiPatch(base, { enabled: !current.enabled, chapter: chapter() }); if (!signal.aborted) draw(data.auto); }
    catch (err) { if (!signal.aborted) toast(err.message, 6000); }
    finally { busy = false; if (!signal.aborted) control.disabled = false; }
  });
  more.addEventListener('click', () => loadPages());
  history.addEventListener('change', () => loadPages(true));
  async function loadRuns() {
    const data = await apiGet(base + '/runs', signal);
    if (signal.aborted) return;
    const selected = history.value; history.replaceChildren(latest);
    for (const run of data.runs) {
      if (run.run_id === current?.run_id) continue;
      const option = el('option', null, new Date(run.created_at * 1000).toLocaleString() + ' · 第 ' + (run.start_chapter + 1) + ' 章起 · ' + run.pages + ' 页');
      option.value = run.run_id; history.appendChild(option);
    }
    history.value = selected;
  }
  async function poll() {
    try {
      const data = await apiGet(base, signal);
      if (signal.aborted) return;
      if (!busy) draw(data.auto);
      if (generation !== data.auto.run_id || total < 0) {
        generation = data.auto.run_id; await loadRuns(); await loadPages(true);
      } else if (!history.value && data.auto.pages > total && rendered >= total) await loadPages();
    } catch (err) { if (!signal.aborted) status.textContent = '读取进度失败：' + err.message; }
    if (!signal.aborted) timer = setTimeout(poll, 2500);
  }
  signal.addEventListener('abort', () => clearTimeout(timer), { once: true });
  poll();
}
