import { el, apiGet, apiPost, apiPatch, toast } from '../util/core.js';

const KINDS = [['solo', '单人镜头'], ['duo', '双人互动'], ['environment', '环境空镜']];
const SHOTS = [['wide', '远景'], ['medium', '中景'], ['portrait', '特写'], ['three_quarter', '侧面近景'], ['reaction', '表情特写'], ['detail', '道具与动作细节']];

export async function mountStoryboards({ area, bookId, signal, scenes = [], boardId = null, automatic = false }) {
  const base = '/api/books/' + encodeURIComponent(bookId) + '/manga/storyboards';
  let board, panels = [], dirty = false, busy = false, generation = 0;
  const root = el('div', 'storyboard-editor'); area.appendChild(root);
  const controls = el('div'), status = el('p', 'manga-note'), editor = el('div'); root.append(controls, status, editor);
  status.setAttribute('role', 'status');
  function button(parent, label, fn) {
    const b = el('button', 'btn', label); b.type = 'button'; parent.append(b);
    b.addEventListener('click', async () => {
      if (busy) return;
      busy = true; b.disabled = true;
      try { await fn(); } catch (err) { if (!signal.aborted) { status.textContent = err.message; toast(err.message, 5000); } }
      finally { busy = false; b.disabled = false; }
    }); return b;
  }
  function field(parent, label, value, change, options = null) {
    const wrap = el('label', 'manga-field'); wrap.append(el('span', null, label));
    const input = el(options ? 'select' : 'textarea'); input.setAttribute('aria-label', label);
    if (options) for (const [id, name] of options) { const o = el('option', null, name); o.value = id; input.append(o); }
    input.value = value || ''; wrap.append(input); parent.append(wrap);
    input.addEventListener('change', () => { dirty = true; change(input.value); status.textContent = '有未保存的修改，请先保存分镜。'; });
    // Mark edits before blur so a click on Generate cannot use stale saved text.
    if (!options) input.addEventListener('input', () => { dirty = true; change(input.value); });
    return input;
  }
  function adopt(value) {
    board = value; panels = structuredClone(value.panels); dirty = false; draw();
    const context = board.web_context;
    if (context && Array.isArray(context.results) && context.results.length) {
      const sources = el('details', 'storyboard-web-sources');
      sources.append(el('summary', null, '联网参考来源（不属于原文证据）'));
      for (const item of context.results.slice(0, 10)) {
        let url;
        try { url = new URL(item.url); } catch { continue; }
        if (!['http:', 'https:'].includes(url.protocol)) continue;
        const link = el('a', null, item.title || url.hostname); link.href = url.href; link.target = '_blank'; link.rel = 'noopener noreferrer';
        sources.append(link, el('p', null, item.snippet || ''));
      }
      if (sources.childElementCount > 1) root.insertBefore(sources, editor);
    }
    status.textContent = `已保存 ${panels.length} 格 · 版本 ${board.revision} · 文本模型 ${board.provider.model}`;
  }
  async function open(id) {
    const token = ++generation;
    const result = await apiGet(base + '/' + encodeURIComponent(id), signal);
    if (!signal.aborted && token === generation) adopt(result.storyboard);
  }
  async function render(indices) {
    if (dirty) throw new Error('请先保存分镜，再开始生成');
    const result = await apiPost(base + '/' + board.id + '/render', { revision: board.revision, panels: indices });
    status.textContent = result.jobs.length + ' 格已加入生成队列，可在下方生成记录查看。';
  }
  function draw() {
    editor.replaceChildren();
    editor.append(el('p', 'manga-note', '先确认事件顺序、人物位置与对白。人物外貌来自参考图；动作与环境使用英文描述，旁白和对白使用中文。双人镜头需要支持多参考图的图像 API。'));
    const actions = el('div', 'manga-actions'); editor.append(actions);
    button(actions, '保存分镜修改', async () => {
      const result = await apiPatch(base + '/' + board.id, { revision: board.revision, panels });
      if (!signal.aborted) adopt(result.storyboard);
    });
    button(actions, automatic ? '确认本页并继续' : '生成已保存整页', async () => {
      if (dirty) throw new Error('请先保存分镜');
      if (automatic) {
        await apiPost(base + '/' + board.id + '/approve', { revision: board.revision });
        status.textContent = '本页已确认，自动漫画将继续；如已暂停，请开启开关。';
      } else await render(panels.map((_, i) => i));
    });
    const cast = board.material.characters.map(c => [c.id, c.name]);
    panels.forEach((panel, index) => {
      const card = el('article', 'storyboard-card'); editor.append(card);
      card.append(el('h4', null, '第 ' + (index + 1) + ' 格'));
      const panelPreview = (board.reference_preview || []).find(item => item.panel === index);
      const previewSubjects = panel.subjects.map(subject => panelPreview?.subjects?.find(item => item.character_id === subject.character_id)).filter(Boolean);
      if (previewSubjects.length) {
        const summary = previewSubjects.map(subject => subject.name + ' · ' + (subject.form ? subject.form.label + ' v' + subject.form.version : '当前形态不可用') +
          (subject.current_reference ? (subject.current_reference.accepted ? ' · 已确认参考图' : ' · 自动参考图草稿，待复核') : ' · 尚无可用参考图')).join('；');
        card.append(el('p', 'manga-note storyboard-reference-summary', '本格实际人物与参考图：' + summary));
      } else if (panel.kind === 'environment') card.append(el('p', 'manga-note storyboard-reference-summary', '本格为环境空镜，不使用人物参考图。'));
      const toolbar = el('div', 'manga-actions'); card.append(toolbar);
      button(toolbar, '前移', () => { if (index) { [panels[index - 1], panels[index]] = [panel, panels[index - 1]]; dirty = true; draw(); } });
      button(toolbar, '后移', () => { if (index + 1 < panels.length) { [panels[index + 1], panels[index]] = [panel, panels[index + 1]]; dirty = true; draw(); } });
      button(toolbar, '移除', () => { if (panels.length < 2) throw new Error('至少保留一格'); panels.splice(index, 1); dirty = true; draw(); });
      if (!automatic) button(toolbar, '仅生成此格', () => render([index]));
      field(card, '画面类型', panel.kind, value => {
        const count = { solo: 1, duo: 2, environment: 0 }[value];
        panel.kind = value;
        panel.subjects = Array.from({ length: count }, (_, i) => panel.subjects[i] || { character_id: cast[i]?.[0] || '', position: count === 1 ? 'center' : i ? 'right' : 'left', pose: '' });
        draw();
      }, KINDS.filter(([kind]) => kind === 'environment' || cast.length >= (kind === 'duo' ? 2 : 1)));
      field(card, '镜头距离', panel.shot, v => { panel.shot = v; }, SHOTS);
      const grid = el('div', 'storyboard-fields'); card.append(grid);
      for (let subjectIndex = 0; subjectIndex < panel.subjects.length; subjectIndex++) {
        const subject = panel.subjects[subjectIndex];
        const group = el('div', 'storyboard-subject'); grid.append(group);
        field(group, '画面人物', subject.character_id, v => { subject.character_id = v; delete subject.reference_id; draw(); }, cast);
        field(group, '人物位置', subject.position, v => { subject.position = v; }, [['left', '左侧'], ['center', '中间'], ['right', '右侧']]);
        field(group, '人物姿势（英文）', subject.pose, v => { subject.pose = v; });
        const selected = previewSubjects.find(item => item.character_id === subject.character_id);
        const refOptions = [['', '自动使用当前已确认图']].concat((selected?.references || []).map(ref => [ref.id, '已确认参考图 · ' + new Date(ref.created_at * 1000).toLocaleString()]));
        const ref = field(group, '人物形态参考图', subject.reference_id || '', value => {
          if (value) subject.reference_id = value; else delete subject.reference_id;
          dirty = true; refreshImage();
        }, refOptions);
        ref.disabled = !selected?.references?.length;
        if (!selected?.references?.length) ref.title = '请先在“人物与参考图”确认当前人物形态的参考图';
        const image = el('img', 'storyboard-reference-image'); image.loading = 'lazy'; image.style.maxWidth = '100%'; image.style.maxHeight = '180px'; image.style.objectFit = 'contain';
        const refreshImage = () => {
          const actual = ref.value ? selected?.references?.find(item => item.id === ref.value) : selected?.current_reference;
          image.hidden = !actual; if (actual) { image.src = actual.url; image.alt = selected.name + ' · ' + selected.form.label + ' · 实际参考图'; }
        };
        refreshImage(); group.append(image);
        group.append(el('p', 'manga-note', selected?.form ? '当前形态：' + selected.form.label + ' · v' + selected.form.version : '保存分镜后可查看该人物在本节的生效形态。'));
      }
      for (const [key, label] of [['action', '可见动作（英文）'], ['environment', '地点与道具（英文）'], ['lighting', '时间与光线（英文）'], ['emotion', '表情（英文）'], ['caption', '中文旁白'], ['dialogue', '原文对白'], ['speaker', '说话人'], ['source_quote', '原文依据']]) {
        field(grid, label, panel[key], v => { panel[key] = v; });
      }
    });
  }
  if (!boardId) {
    controls.append(el('p', 'manga-note', '一次编排整组镜头，保存后可逐格修改。出图前请在“人物与参考图”确认所用人物。'));
    let selected = scenes[0]?.id;
    const scene = field(controls, '编排场景', selected, v => { selected = v; }, scenes.map(s => [s.id, '第 ' + (s.chapter_id + 1) + ' 章 · ' + s.plan.summary]));
    button(controls, '生成整页分镜预览', async () => {
      if (!scene.value) throw new Error('请先完成章节分析');
      if (dirty) throw new Error('请先保存当前修改');
      status.textContent = '正在编排整页分镜…';
      const result = await apiPost(base, { scene_id: scene.value });
      if (!signal.aborted) adopt(result.storyboard);
    });
    const saved = await apiGet(base, signal);
    if (signal.aborted) return;
    if (saved.storyboards.length) {
      const select = field(controls, '已保存的分镜', '', () => {}, [['', '选择历史方案'], ...saved.storyboards.map((p, i) => [p.id, '方案 ' + (i + 1) + ' · 版本 ' + p.revision])]);
      button(controls, '打开所选方案', () => { if (dirty) throw new Error('请先保存修改'); if (select.value) return open(select.value); });
    }
  } else await open(boardId);
}
