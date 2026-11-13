import { el, apiGet, apiPost, apiPatch, toast } from '../util/core.js';

const STATES = { queued: '排队中', running: '生成中', needs_review: '待复核', done: '相似度通过', approved: '人工已确认', error: '生成失败', stale: '已过期，需重新生成', cancelled: '已取消' };
const SHOTS = { portrait: '正面特写', medium: '中景', wide: '全身远景', three_quarter: '侧面近景', reaction: '表情特写' };
const APPEARANCE = { age: '年龄', gender: '性别', hair_style: '发型', hair_color: '发色', eye_color: '瞳色', skin: '肤色', outfit: '服装', accessories: '配饰' };

export async function mountManga({ area, bookId, signal }) {
  const base = '/api/books/' + encodeURIComponent(bookId);
  let forms = [], scenes = [], config, selected = '', timer, busy = false;
  const root = el('div', 'manga-studio');
  area.replaceChildren(root);
  const note = el('p', 'manga-note', '每格使用一个人物的一张已确认参考图。先在 AI 面板分析片段并冻结人物身份。');
  root.appendChild(note);
  const tabs = el('div', 'manga-tabs'); root.appendChild(tabs);
  const content = el('div'); root.appendChild(content);
  const jobsHead = el('h3', 'ai-section-title', '生成记录'); root.appendChild(jobsHead);
  const jobsArea = el('div', 'manga-jobs'); root.appendChild(jobsArea);
  let active = 'generate', lastJobs = '';
  signal.addEventListener('abort', () => clearTimeout(timer), { once: true });

  function button(parent, label, fn, cls = '') {
    const b = el('button', 'btn ' + cls, label); b.type = 'button'; parent.appendChild(b);
    b.addEventListener('click', async () => {
      if (busy) return;
      busy = true; b.disabled = true;
      try { await fn(); } catch (err) { if (!signal.aborted) toast(err.message || '操作失败', 4000); }
      finally { busy = false; b.disabled = false; }
    });
    return b;
  }
  function field(parent, label, value = '', type = 'text') {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, label));
    const input = el(type === 'textarea' ? 'textarea' : 'input');
    if (type !== 'textarea') input.type = type;
    input.value = value; input.setAttribute('aria-label', label); wrap.appendChild(input); parent.appendChild(wrap);
    return input;
  }
  function select(parent, label, options, value) {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, label));
    const input = el('select'); input.setAttribute('aria-label', label);
    for (const [id, text] of options) { const option = el('option', null, text); option.value = id; input.appendChild(option); }
    if (value != null) input.value = value;
    wrap.appendChild(input); parent.appendChild(wrap); return input;
  }
  function image(parent, url, alt) {
    if (!url) return;
    const link = el('a', 'manga-preview'); link.href = url; link.target = '_blank'; link.rel = 'noopener';
    const img = el('img'); img.src = url; img.alt = alt; img.loading = 'lazy'; link.appendChild(img); parent.appendChild(link);
  }
  async function load() {
    const [f, c, sc] = await Promise.all([apiGet(base + '/manga/forms', signal), apiGet(base + '/manga/config', signal), apiGet(base + '/ai/scenes', signal)]);
    forms = f.forms; config = c.config; scenes = sc.scenes;
    if (!forms.some(f => f.id === selected)) selected = forms[0]?.id || '';
  }
  function render() {
    if (signal.aborted) return;
    tabs.replaceChildren(); content.replaceChildren();
    for (const [id, label] of [['generate', '分镜'], ['refs', '人物与参考图'], ['config', '出图设置']]) {
      const tab = button(tabs, label, () => { active = id; render(); });
      tab.setAttribute('aria-pressed', String(active === id));
    }
    if (active === 'generate') renderGenerate();
    if (active === 'refs') renderRefs();
    if (active === 'config') renderConfig();
  }
  function renderGenerate() {
    if (!scenes.length || !forms.length) { content.appendChild(el('p', 'manga-note', '还没有场景或人物卡。请返回 AI 面板分析片段。')); return; }
    const scene = select(content, '场景', scenes.map(s => [s.id, '第 ' + (s.chapter_id + 1) + ' 章 · ' + s.plan.summary]));
    const form = select(content, '人物形态', forms.map(f => [f.id, f.label + ' · v' + f.version]), selected);
    const hint = el('p', 'manga-note'); content.appendChild(hint);
    const update = () => {
      selected = form.value;
      const f = forms.find(f => f.id === selected), sc = scenes.find(s => s.id === scene.value);
      hint.textContent = !f?.locked ? '人物身份尚未冻结' : !f.refs.some(r => r.accepted && r.form_version === f.version) ? '请先在“人物与参考图”中确认一张参考图' : sc?.plan.character_ids?.length > 1 ? '此场景有多个人物，请先拆成单人场景' : '已准备好。生成后会与所选参考图比较人物相似度。';
    };
    scene.addEventListener('change', update); form.addEventListener('change', update); update();
    const shot = select(content, '镜头', Object.entries(SHOTS), 'medium');
    const action = field(content, '动作与表情', '', 'textarea'); action.maxLength = 1200;
    const para = field(content, '场景内段落（可选，从 1 开始）', '', 'number'); para.min = '1'; para.step = '1';
    const flash = field(content, '回忆 / 闪回（允许使用较早形态）', '', 'checkbox');
    button(content, '加入生成队列', async () => {
      const payload = { scene_id: scene.value, form_id: form.value, shot: shot.value, action: action.value, flashback: flash.checked, request_id: crypto.randomUUID() };
      if (para.value) payload.para = Number(para.value) - 1;
      await apiPost(base + '/manga/jobs', payload); toast('已加入生成队列'); await refreshJobs();
    }, 'btn-primary');
  }
  function renderRefs() {
    if (!forms.length) { content.appendChild(el('p', 'manga-note', '尚无人物卡，请先在 AI 面板分析片段。')); return; }
    const pick = select(content, '当前形态', forms.map(f => [f.id, f.label + ' · v' + f.version]), selected);
    pick.addEventListener('change', () => { selected = pick.value; render(); });
    const f = forms.find(f => f.id === selected);
    content.appendChild(el('p', 'ai-identity', (f.locked ? '已冻结 · ' : '草稿 · ') + f.identity_block));
    const actions = el('div', 'manga-actions'); content.appendChild(actions);
    button(actions, '生成参考图', async () => {
      await apiPost(base + '/manga/jobs', { kind: 'reference', form_id: f.id, request_id: crypto.randomUUID() });
      toast('参考图已排队，生成后请确认'); await refreshJobs();
    });
    const uploadLabel = el('label', 'manga-field'); uploadLabel.appendChild(el('span', null, '上传参考图（PNG / JPEG / WebP）'));
    const file = el('input'); file.type = 'file'; file.accept = 'image/png,image/jpeg,image/webp'; uploadLabel.appendChild(file); content.appendChild(uploadLabel);
    file.addEventListener('change', async () => {
      if (!file.files[0]) return;
      file.disabled = true;
      try {
        if (file.files[0].size > 16 * 1024 * 1024) throw new Error('图片不能超过 16 MB');
        const response = await fetch(base + '/manga/forms/' + encodeURIComponent(f.id) + '/refs', { method: 'POST', body: file.files[0], signal });
        const result = await response.json(); if (!response.ok) throw new Error(result.error?.message || '上传失败');
        await load(); render();
      } catch (err) { if (!signal.aborted) toast(err.message, 4000); } finally { file.disabled = false; }
    });
    const refs = el('div', 'manga-refs'); content.appendChild(refs);
    for (const r of f.refs) {
      const card = el('article', 'manga-ref'); refs.appendChild(card); image(card, r.url, f.label + '参考图');
      card.appendChild(el('p', 'manga-note', (r.accepted ? '已确认' : '未确认') + ' · v' + r.form_version));
      const b = button(card, r.accepted ? '撤销确认' : '确认参考图', async () => {
        await apiPatch(base + '/manga/refs/' + r.id, { accepted: !r.accepted }); await load(); render(); await refreshJobs();
      });
      b.disabled = !f.locked || r.form_version !== f.version;
    }
    const editor = el('details', 'manga-editor'); editor.appendChild(el('summary', null, '编辑身份 / 添加新形态')); content.appendChild(editor);
    editor.appendChild(el('p', 'manga-note', '外貌描述建议使用英文。保存会冻结新版本，旧版图片将标为过期。新增形态按章节、段落生效。'));
    const label = field(editor, '形态名称', f.label);
    const values = {};
    for (const [key, title] of Object.entries(APPEARANCE)) values[key] = field(editor, title, f.card[key] || '');
    const mode = select(editor, '保存方式', [['edit', '修改当前身份并升版'], ['new', '添加新形态']], 'edit');
    const chapter = field(editor, '生效章节（从 1 开始）', f.start_ch + 1, 'number'); chapter.min = '1';
    const para = field(editor, '生效段落（从 1 开始）', f.start_para + 1, 'number'); para.min = '1';
    button(editor, '保存并冻结', async () => {
      const card = Object.fromEntries(Object.entries(f.card).filter(([key]) => !['evidence', 'variants', 'identity_history'].includes(key)));
      for (const [key, input] of Object.entries(values)) card[key] = input.value;
      if (mode.value === 'new') {
        const result = await apiPost(base + '/manga/forms', { character_id: f.character_id, form_key: crypto.randomUUID(), label: label.value, card, start_ch: Number(chapter.value) - 1, start_para: Number(para.value) - 1, reversible: false });
        await apiPost(base + '/manga/forms/' + result.id + '/actions/freeze', {}); selected = result.id;
      } else if (f.form_key === 'base') {
        if (f.locked) await apiPost(base + '/ai/characters/' + f.character_id + '/unfreeze', { confirm: true });
        await apiPatch(base + '/ai/characters/' + f.character_id, { card });
        await apiPost(base + '/ai/characters/' + f.character_id + '/freeze', {});
      } else {
        if (f.locked) await apiPost(base + '/manga/forms/' + f.id + '/actions/unfreeze', { confirm: true });
        await apiPatch(base + '/manga/forms/' + f.id, { label: label.value, card, start_ch: Number(chapter.value) - 1, start_para: Number(para.value) - 1 });
        await apiPost(base + '/manga/forms/' + f.id + '/actions/freeze', {});
      }
      await load(); render(); await refreshJobs(); toast('身份已保存并冻结');
    });
  }
  function renderConfig() {
    const provider = select(content, '图像后端', [['comfyui', 'ComfyUI / MOSS'], ['mock', '占位图（仅验证流程）']], config.provider);
    const inputs = {};
    const labels = { base_url: '服务地址', unet: '绘图模型', clip: '9B 文本编码器', vae: '图像解码模型', edit_lora: '参考图编辑模型', width: '宽度', height: '高度', steps: '采样步数', cfg: '提示词强度', sampler: '采样器', scheduler: '调度器', seed: '基础种子', face_threshold: '相似度阈值', max_attempts: '最多生成次数', timeout: '等待超时（秒）', style: '画风', negative: '排除内容', face_python: '人脸校验 Python 路径（可选）', face_models: '人脸模型目录（可选）' };
    const numbers = new Set(['width', 'height', 'steps', 'cfg', 'seed', 'face_threshold', 'max_attempts', 'timeout']);
    for (const [key, label] of Object.entries(labels)) {
      inputs[key] = field(content, label, config[key], numbers.has(key) ? 'number' : ['style', 'negative'].includes(key) ? 'textarea' : 'text');
      if (numbers.has(key)) inputs[key].step = ['cfg', 'face_threshold'].includes(key) ? '0.01' : '1';
    }
    const feedback = el('p', 'manga-note'); content.appendChild(feedback);
    button(content, '保存设置', async () => {
      const payload = { provider: provider.value };
      for (const [key, input] of Object.entries(inputs)) payload[key] = numbers.has(key) ? Number(input.value) : input.value;
      config = (await apiPatch(base + '/manga/config', payload)).config; toast('出图设置已保存');
    }, 'btn-primary');
    button(content, '检测已保存的连接与模型', async () => {
      const result = await apiPost(base + '/manga/probe', {});
      feedback.textContent = result.ok ? (result.note || '连接正常，MOSS 单参考图编辑可用') : '缺少：' + result.missing.join('、');
    });
  }
  async function refreshJobs() {
    const data = await apiGet(base + '/manga/jobs', signal);
    const signature = JSON.stringify(data.jobs);
    if (signature === lastJobs || signal.aborted) return;
    lastJobs = signature; jobsArea.replaceChildren();
    for (const job of data.jobs) {
      const card = el('article', 'manga-job'); card.dataset.job = job.id; jobsArea.appendChild(card);
      card.appendChild(el('h4', null, (job.kind === 'reference' ? '参考图' : '分镜') + ' · ' + (STATES[job.status] || job.status)));
      image(card, job.url, job.kind === 'reference' ? '参考图候选' : '生成的分镜');
      card.appendChild(el('p', 'manga-note', job.face_score == null ? '尚无有效人脸相似度' : '人脸相似度 ' + job.face_score.toFixed(3) + ' / 阈值 ' + job.snapshot.config.face_threshold));
      if (job.error) card.appendChild(el('p', 'manga-note', job.error));
      const actions = el('div', 'manga-actions'); card.appendChild(actions);
      if (job.url && ['done', 'needs_review', 'approved'].includes(job.status)) button(actions, '存为参考图候选', async () => {
        await apiPost(base + '/manga/jobs/' + job.id + '/reference', {}); await load(); active = 'refs'; selected = job.form_id; render();
      });
      for (const [action, label] of [['cancel', '取消'], ['retry', '继续失败任务'], ['reroll', '换种子重画'], ['approve', '人工确认此图']]) {
        if (action === 'cancel' && !['queued', 'running', 'error'].includes(job.status)) continue;
        if (action === 'retry' && job.status !== 'error') continue;
        if (action === 'reroll' && ['queued', 'running'].includes(job.status)) continue;
        if (action === 'approve' && (job.status !== 'needs_review' || job.kind !== 'panel')) continue;
        button(actions, label, async () => { await apiPost(base + '/manga/jobs/' + job.id + '/' + action, {}); await refreshJobs(); });
      }
      const detail = el('details', 'manga-snapshot'); detail.appendChild(el('summary', null, '提示词、种子与生成记录'));
      detail.appendChild(el('pre', null, JSON.stringify({ seed: job.snapshot.seed, prompt: job.snapshot.prompt, config_hash: job.config_hash, attempts: job.history.map(h => ({ seed: h.seed, result: h.result, prompt_id: h.prompt_id, error: h.error })) }, null, 2)));
      card.appendChild(detail);
    }
    if (!data.jobs.length) jobsArea.appendChild(el('p', 'manga-note', '尚无生成任务'));
    // New reference candidates appear in the form gallery without replacing a user's edits.
    if (active === 'refs' && !content.contains(document.activeElement)) { await load(); render(); }
  }
  async function poll() {
    try { await refreshJobs(); } catch (err) { if (!signal.aborted) jobsHead.textContent = '生成记录 · 连接中断，正在重试'; }
    if (!signal.aborted) timer = setTimeout(poll, 2500);
  }
  try { await load(); render(); await poll(); }
  catch (err) { if (!signal.aborted) content.textContent = '加载失败：' + err.message; }
}
