import { el, apiGet, apiPost, toast } from '../util/core.js';

const STAGES = { analysis: '章节分析', reduction: '章节大意合并', memory: '章节连续性记忆', status: '任务状态' };

export function mountBookAnalysis({ area, bookId, signal }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/ai/analysis';
  const status = el('p', 'manga-note', '正在读取整书分析进度…');
  const progress = el('progress'); progress.setAttribute('aria-label', '整书阅读进度'); progress.style.width = '100%';
  const actions = el('div', 'manga-actions');
  const start = el('button', 'btn btn-primary', '开始 / 继续整书分析');
  const pause = el('button', 'btn', '暂停分析');
  const diagnoseButton = el('button', 'btn', '诊断');
  const exportLink = el('a', 'btn', '导出诊断包');
  exportLink.href = base + '/diagnostics/export'; exportLink.setAttribute('download', '');
  exportLink.title = '下载脱敏任务日志、配置类别与失败样本';
  const failureBox = el('div', 'automatic-diagnosis-item'); failureBox.hidden = true;
  const failureTitle = el('strong'); const failureDetail = el('p', 'manga-note'); const failureMeta = el('p', 'manga-note');
  failureBox.append(failureTitle, failureDetail, failureMeta);
  const diagnosis = el('section', 'automatic-diagnosis'); diagnosis.hidden = true; diagnosis.setAttribute('aria-live', 'polite');
  const diagnosisBody = el('div'); diagnosis.append(diagnosisBody);
  const logs = el('div', 'automatic-logs'); logs.setAttribute('role', 'log'); logs.setAttribute('aria-label', '整书分析任务日志');
  const list = el('div'); let timer, signature = '';
  area.append(el('p', 'manga-note', '自动阅读全书，逐章保存大意、人物卡和可绘制场景。长章节会分段处理；关闭软件后可继续。外貌有变化时保留原文证据，冻结的身份不会被覆盖。'),
    status, progress, failureBox, actions, diagnosis, logs, list);
  actions.append(start, pause, diagnoseButton, exportLink);
  const states = { idle: '尚未开始', queued: '排队中', running: '阅读中', paused: '已暂停', error: '已中断', done: '整书分析完成' };
  function showFailure(failure) {
    failureBox.hidden = !failure;
    if (!failure) return;
    failureTitle.textContent = failure.title || '整书分析中断';
    const usage = failure.usage || {};
    const pieces = [failure.message, failure.suggestion].filter(Boolean);
    failureDetail.textContent = pieces.join(' · ');
    const meta = [failure.stage ? '阶段：' + (STAGES[failure.stage] || failure.stage) : '',
      failure.model ? '模型：' + failure.model : '',
      failure.http_status ? 'HTTP ' + failure.http_status : '',
      failure.finish_reason ? 'finish_reason=' + failure.finish_reason : '',
      usage.completion_tokens != null ? '完成 ' + usage.completion_tokens + ' token' : '',
      usage.reasoning_tokens ? '思考 ' + usage.reasoning_tokens + ' token' : ''].filter(Boolean);
    failureMeta.textContent = meta.join(' · ');
    failureBox.dataset.level = 'error';
  }
  function draw(data) {
    if (signal.aborted) return;
    const a = data.analysis;
    status.textContent = (states[a.status] || a.status) + ' · 已完成 ' + a.completed + ' / ' + a.total + ' 章'
      + (a.status === 'running' && a.current_chapter != null ? ' · 第 ' + (a.current_chapter + 1) + ' 章，分段 ' + a.current_part + ' / ' + a.total_parts : '');
    progress.max = Math.max(1, a.total); progress.value = a.completed;
    start.disabled = ['queued', 'running'].includes(a.status);
    pause.disabled = !['queued', 'running'].includes(a.status);
    showFailure(a.failure);
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
  async function diagnose() {
    diagnoseButton.disabled = true;
    diagnosis.hidden = false;
    diagnosisBody.replaceChildren(el('p', 'manga-note', '正在读取诊断信息（不会调用模型）…'));
    try {
      const data = await apiGet(base + '/diagnose', signal);
      if (signal.aborted) return;
      diagnosisBody.replaceChildren();
      const heading = el('h3', 'ai-section-title', '分析诊断');
      diagnosisBody.appendChild(heading);
      for (const check of data.checks || []) {
        const row = el('article', 'automatic-diagnosis-item'); row.dataset.level = check.level;
        row.append(el('strong', null, check.title), el('p', 'manga-note', check.detail));
        diagnosisBody.appendChild(row);
      }
      if (data.failure) showFailure(data.failure);
      const summary = data.config || {};
      diagnosisBody.appendChild(el('p', 'manga-note',
        '当前来源：' + (summary.source || '未配置') + ' · 最大 Token ' + (summary.max_tokens ?? '—')
        + '（推理模型建议 ' + (summary.recommended_max_tokens ?? 16000) + '，上限 ' + (summary.max_tokens_limit ?? 32768) + '）'));
      if (data.samples?.length) {
        diagnosisBody.appendChild(el('p', 'manga-note',
          '已保留 ' + data.samples.length + ' 份失败样本（明文只在本机 data/diagnostics，导出时脱敏）。'));
      }
      logs.replaceChildren();
      logs.hidden = !(data.logs || []).length;
      for (const item of data.logs || []) {
        const row = el('p', 'automatic-log'); row.dataset.level = item.level;
        row.append(el('time', null, new Date(item.created_at * 1000).toLocaleTimeString()),
          el('span', null, (STAGES[item.stage] || '任务状态') + ' · ' + item.message));
        logs.appendChild(row);
      }
      if (!logs.children.length) logs.hidden = true;
    } catch (err) {
      if (!signal.aborted) diagnosisBody.replaceChildren(el('p', 'manga-note', '诊断失败：' + err.message));
    } finally { diagnoseButton.disabled = false; }
  }
  diagnoseButton.addEventListener('click', diagnose);
  async function poll() {
    try { draw(await apiGet(base, signal)); }
    catch (err) { if (!signal.aborted) status.textContent = '读取进度失败：' + err.message; }
    if (!signal.aborted) timer = setTimeout(poll, 2500);
  }
  signal.addEventListener('abort', () => clearTimeout(timer), { once: true });
  poll();
}
