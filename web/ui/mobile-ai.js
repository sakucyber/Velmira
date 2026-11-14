import { el, apiGet, apiPost, apiPatch, apiDelete, fmtTime } from '../util/core.js';

const BASE = '/api/mobile-ai';
const STRATEGIES = [['priority', '按顺序降级'], ['round_robin', '轮询分摊'], ['least_busy', '优先空闲']];
const PRESETS = {
  agnes: { label: 'Agnes AI', base_url: 'https://apihub.agnes-ai.com/v1', kind: 'image', models: 'agnes-image-2.5-flash, agnes-image-2.1-flash, agnes-image-2.0-flash', rpm: 0, max_concurrency: 1 },
  luna: { label: '文本中转站', base_url: 'https://kndnadi.org/v1', kind: 'text', models: 'gpt-6-luna', rpm: 0, max_concurrency: 5 },
  public: { label: '公益站文本', base_url: 'https://api.pie-xian.com/v1', kind: 'text', models: 'qwen3.8-27b, gpt-oss-120b', rpm: 25, max_concurrency: 8 },
  bilibili: { label: 'Bilibili 免费翻译', base_url: 'https://index-translate.bilibili.com/v1', kind: 'text', models: 'Index-Translate-35B-A3B', rpm: 0, max_concurrency: 2 },
  image: { label: '中转站生图', base_url: 'https://api.pie-xian.com/v1', kind: 'image', models: '', rpm: 25, max_concurrency: 8 },
};

function styles() {
  if (document.getElementById('mobile-ai-style')) return;
  const link = el('link'); link.id = 'mobile-ai-style'; link.rel = 'stylesheet'; link.href = '/styles/mobile-ai.css'; document.head.append(link);
}
function field(parent, label, value = '', type = 'text') {
  const wrap = el('label', 'mai-field'); wrap.append(el('span', null, label));
  const node = el(type === 'textarea' ? 'textarea' : 'input');
  if (type !== 'textarea') node.type = type;
  if (type === 'checkbox') { node.checked = !!value; wrap.classList.add('mai-check'); } else node.value = value ?? '';
  node.setAttribute('aria-label', label); wrap.append(node); parent.append(wrap); return node;
}
function select(parent, label, options, value) {
  const wrap = el('label', 'mai-field'); wrap.append(el('span', null, label)); const node = el('select'); node.setAttribute('aria-label', label);
  for (const [id, title] of options) { const option = el('option', null, title); option.value = id; node.append(option); }
  if (value != null) node.value = value; wrap.append(node); parent.append(wrap); return node;
}
function card(parent, title, help) { const node = el('section', 'mai-card'); if (title) node.append(el('h3', null, title)); if (help) node.append(el('p', 'settings-help', help)); parent.append(node); return node; }
function report(parent) { const node = el('p', 'mai-report'); node.setAttribute('role', 'status'); parent.append(node); return (message, error = false) => { node.textContent = message; node.dataset.error = String(error); }; }
function action(parent, title, work, signal, tell, primary = false) {
  const button = el('button', 'btn' + (primary ? ' btn-primary' : ''), title); button.type = 'button'; parent.append(button);
  button.addEventListener('click', async () => {
    if (button.disabled || signal.aborted) return; button.disabled = true;
    try { await work(); } catch (error) { if (!signal.aborted) tell(error.message || '操作失败', true); }
    finally { if (button.isConnected) button.disabled = false; }
  }); return button;
}
function number(parent, label, value, min, max) { const node = field(parent, label, value, 'number'); node.min = min; node.max = max; node.step = 1; return node; }
const clone = value => JSON.parse(JSON.stringify(value));

export async function mountMobileAISettings({ area, signal, section, bookId }) {
  styles(); area.classList.add('mobile-ai'); area.replaceChildren(el('p', 'settings-help', '正在读取手机 AI 设置…'));
  let settings = (await apiGet(BASE + '/settings', signal)).settings;
  if (signal.aborted) return;
  area.replaceChildren();
  const save = async patch => { settings = (await apiPatch(BASE + '/settings', { ...patch, revision: settings.revision }, signal)).settings; return settings; };
  const rerender = () => mountMobileAISettings({ area, signal, section, bookId });
  if (section === 'models') return models();
  if (section === 'prompts') return prompts();
  if (section === 'memory') return memories();
  if (section === 'search') return search();

  function models() {
    area.append(el('h3', 'settings-heading', '手机模型库与自动降级'), el('p', 'settings-help', '手机直接连接你配置的 API，无需电脑。密钥只保存在当前应用的私有存储中。文本与生图分别设置模型池；翻译和 AI 解释复用文本模型池。可用快速填写添加 Bilibili 免费翻译接口。'));
    const saved = card(area, '已保存连接');
    const editorDetails = el('details', 'mai-card mai-connection-editor'); editorDetails.open = !settings.connections.length;
    const editorTitle = el('summary', null, '添加厂商连接'), editor = el('div', 'mai-connection-editor-body'); editorDetails.append(editorTitle, editor); area.append(editorDetails);
    const preset = select(editor, '快速填写', [['', '自定义 API'], ...Object.entries(PRESETS).map(([id, v]) => [id, v.label])]);
    const label = field(editor, '连接名称'); const kind = select(editor, '连接类型', [['text', '文本'], ['image', '生图']], 'text');
    const address = field(editor, 'API 地址（含 /v1 等前缀）', '', 'url'); address.placeholder = 'http://192.168.1.10:8000/v1 或 https://example.com/v1';
    const key = field(editor, 'API 密钥（留空保留）', '', 'password'); key.autocomplete = 'new-password';
    const names = field(editor, '模型名称（逗号或换行分隔）', '', 'textarea'); names.rows = 3;
    const row = el('div', 'mai-grid'); editor.append(row); const rpm = number(row, '每分钟请求上限（0 为不限）', 0, 0, 1000); const concurrency = number(row, '最大并发', 1, 1, 8);
    let editing = null;
    const tell = report(editor), actions = el('div', 'mai-actions'); editor.append(actions);
    const reset = () => { editing = null; label.value = address.value = key.value = names.value = ''; kind.disabled = false; kind.value = 'text'; rpm.value = 0; concurrency.value = 1; editorTitle.textContent = '添加厂商连接'; };
    preset.addEventListener('change', () => { const p = PRESETS[preset.value]; if (!p) return; reset(); label.value = p.label; address.value = p.base_url; names.value = p.models; kind.value = p.kind; rpm.value = p.rpm; concurrency.value = p.max_concurrency; });
    action(actions, '保存厂商连接', async () => {
      const modelNames = [...new Set(names.value.split(/[,，\n]/).map(s => s.trim()).filter(Boolean))];
      if (!label.value.trim() || !address.value.trim() || !modelNames.length) throw new Error('请填写连接名称、地址和至少一个模型名称');
      const next = settings.connections.map(clone); const entry = { label: label.value.trim(), kind: kind.value, base_url: address.value.trim(), models: modelNames, api_key: key.value, rpm: Number(rpm.value), max_concurrency: Number(concurrency.value) };
      if (editing) { entry.id = editing.id; next[next.findIndex(c => c.id === editing.id)] = entry; } else next.push(entry);
      await save({ connections: next }); key.value = ''; await rerender();
    }, signal, tell, true);
    action(actions, '取消编辑', async () => { reset(); tell(''); editorDetails.open = !settings.connections.length; }, signal, tell);
    if (!settings.connections.length) saved.append(el('p', 'settings-help', '还没有连接。可先选快速填写，再填密钥并保存。'));
    for (const connection of settings.connections) {
      const item = el('article', 'mai-connection'); item.append(el('strong', null, connection.label), el('small', null, (connection.kind === 'text' ? '文本' : '生图') + ' · ' + (connection.key_configured ? '密钥已保存' : '无密钥') + ' · ' + connection.base_url), el('p', null, connection.models.join('、')));
      const buttons = el('div', 'mai-actions'); item.append(buttons); const feedback = report(item);
      action(buttons, '编辑连接', async () => { editing = connection; label.value = connection.label; kind.value = connection.kind; kind.disabled = true; address.value = connection.base_url; key.value = ''; names.value = connection.models.join('\n'); rpm.value = connection.rpm; concurrency.value = connection.max_concurrency; editorTitle.textContent = '编辑厂商连接'; editorDetails.open = true; editorDetails.scrollIntoView({ block: 'start' }); }, signal, feedback);
      action(buttons, '删除连接', async () => {
        if (!item.querySelector('.mai-confirm')) {
          const confirm = el('div', 'mai-actions mai-confirm'); item.append(confirm, el('p', 'settings-help', '删除后会从模型池移除，并删除此连接在手机保存的密钥。'));
          action(confirm, '确认删除此连接', async () => { const patch = { connections: settings.connections.filter(c => c.id !== connection.id) }; for (const k of ['text', 'image']) patch[k] = { ...settings[k], models: settings[k].models.filter(m => m.connection_id !== connection.id) }; for (const k of ['text', 'image']) if (!patch[k].models.length) patch[k].enabled = false; await save(patch); await rerender(); }, signal, feedback);
        }
      }, signal, feedback); saved.append(item);
    }
    for (const k of ['text', 'image']) pool(k);
  }
  function pool(kind) {
    const name = kind === 'text' ? '文本' : '生图', config = clone(settings[kind]); let members = config.models.map(clone);
    const root = card(area, name + '模型池', '拖动 ⠿ 调整优先顺序，也可使用上移/下移按钮。失败自动尝试其他模型；每个连接共享并发与 RPM 上限。'); root.dataset.mobilePool = kind;
    const enabled = field(root, '启用' + name + '模型池', config.enabled, 'checkbox');
    const strategy = select(root, name + '调度方式', STRATEGIES, config.strategy);
    const list = el('ol', 'mai-pool'); root.append(list); const add = el('div', 'mai-actions'); root.append(add);
    const options = settings.connections.filter(c => c.kind === kind).flatMap(c => c.models.map(model => ({ connection_id: c.id, model, label: c.label + ' · ' + model })));
    const choice = select(add, '添加到' + name + '池', options.map((m, i) => [String(i), m.label]));
    const tell = report(root);
    const move = (from, to) => { if (to < 0 || to >= members.length || from === to) return; const [item] = members.splice(from, 1); members.splice(to, 0, item); render(); tell('顺序已调整，保存后生效'); };
    function render() {
      list.replaceChildren(); if (!members.length) list.append(el('li', 'settings-help', '尚未选择模型。'));
      members.forEach((m, index) => {
        const row = el('li', 'mai-pool-member'); row.dataset.poolIndex = index;
        const handle = el('button', 'btn mai-grip', '⠿'); handle.type = 'button'; handle.setAttribute('aria-label', name + '模型：拖动第 ' + (index + 1) + ' 个'); row.append(handle);
        handle.addEventListener('pointerdown', event => {
          if (event.button !== 0) return; event.preventDefault(); handle.setPointerCapture(event.pointerId); row.classList.add('dragging'); let target = index;
          const track = e => { const all = [...list.children]; target = all.reduce((best, node, i) => Math.abs(e.clientY - (node.getBoundingClientRect().top + node.offsetHeight / 2)) < Math.abs(e.clientY - (all[best].getBoundingClientRect().top + all[best].offsetHeight / 2)) ? i : best, 0); };
          const end = () => { handle.removeEventListener('pointermove', track); handle.removeEventListener('pointerup', end); handle.removeEventListener('pointercancel', cancel); row.classList.remove('dragging'); move(index, target); };
          const cancel = () => { target = index; end(); }; handle.addEventListener('pointermove', track); handle.addEventListener('pointerup', end); handle.addEventListener('pointercancel', cancel);
        });
        const text = el('div', 'mai-member-text'); text.append(el('strong', null, (index + 1) + '. ' + m.model), el('small', null, settings.connections.find(c => c.id === m.connection_id)?.label || '连接已移除')); row.append(text);
        const controls = el('div', 'mai-actions'); row.append(controls);
        const up = action(controls, '↑', async () => move(index, index - 1), signal, tell); up.disabled = index === 0; up.setAttribute('aria-label', name + '模型上移 ' + m.model);
        const down = action(controls, '↓', async () => move(index, index + 1), signal, tell); down.disabled = index === members.length - 1; down.setAttribute('aria-label', name + '模型下移 ' + m.model);
        action(controls, '×', async () => { members.splice(index, 1); render(); }, signal, tell).setAttribute('aria-label', name + '模型移除 ' + m.model); list.append(row);
      });
    }
    render();
    action(add, '添加模型', async () => { const item = options[Number(choice.value)]; if (!item) throw new Error('请先添加对应类型的厂商连接'); if (members.some(m => m.connection_id === item.connection_id && m.model === item.model)) throw new Error('此模型已在池中'); members.push({ connection_id: item.connection_id, model: item.model }); render(); }, signal, tell);
    const grid = el('div', 'mai-grid'); root.append(grid); const timeout = number(grid, name + '超时（秒）', config.timeout ?? 120, 5, 300); const cooldown = number(grid, name + '失败冷却（秒）', config.cooldown ?? 30, 1, 600);
    const actions = el('div', 'mai-actions'); root.append(actions);
    action(actions, '保存' + name + '模型池', async () => { await save({ [kind]: { ...config, enabled: enabled.checked, strategy: strategy.value, models: members, timeout: Number(timeout.value), cooldown: Number(cooldown.value) } }); tell(name + '模型池已保存'); }, signal, tell, true);
    action(actions, kind === 'text' ? '检测文本连接' : '检测生图连接', async () => { tell('正在检测已保存的模型池…'); const result = await apiPost(BASE + '/probe', { kind }, signal); tell(result.model + ' 连接正常' + (kind === 'image' ? '（仅检测模型列表，不生成图片）' : '')); }, signal, tell);
  }
  function prompts() {
    const root = card(area, '手机 AI 提示词', '用于当前手机发出的请求，保存后即时生效。'); const text = field(root, '文本提示词补充', settings.prompts.text, 'textarea'); text.rows = 6; text.maxLength = 6000;
    const image = field(root, '生图提示词补充', settings.prompts.image, 'textarea'); image.rows = 5; image.maxLength = 3000;
    const negative = field(root, '生图负向提示词', settings.prompts.negative, 'textarea'); negative.rows = 4; negative.maxLength = 2000; const tell = report(root);
    action(root, '保存提示词', async () => { await save({ prompts: { text: text.value, image: image.value, negative: negative.value } }); tell('提示词已保存'); }, signal, tell, true);
  }
  async function memories() {
    const config = card(area, '手机长期记忆', '保存人物人格、常用梗和世界观，供文本与生图请求引用。支持全局或当前作品；数据保存在手机。');
    const enabled = field(config, '启用记忆引用', settings.memory.enabled, 'checkbox'); const grid = el('div', 'mai-grid'); config.append(grid);
    const items = number(grid, '最多引用条数', settings.memory.max_items, 1, 30), chars = number(grid, '引用字数上限', settings.memory.max_chars, 500, 16000); const tell = report(config);
    action(config, '保存记忆设置', async () => { await save({ memory: { enabled: enabled.checked, max_items: Number(items.value), max_chars: Number(chars.value) } }); tell('记忆设置已保存'); }, signal, tell);
    const editor = card(area, '新增记忆'); const title = field(editor, '记忆标题'), kind = select(editor, '记忆类型', [['personality', '人格与性格'], ['meme', '梗与表达'], ['world', '世界观'], ['note', '其他']], 'personality');
    const books = (await apiGet('/api/books', signal)).books; const scope = select(editor, '记忆作品范围', [['', '全局：全部作品'], ...books.map(b => [b.id, b.title])], bookId || '');
    const content = field(editor, '记忆内容', '', 'textarea'); content.rows = 5; content.maxLength = 10000; const tags = field(editor, '记忆标签（逗号分隔）'); let editing = null; const feedback = report(editor);
    action(editor, '保存记忆', async () => { if (!title.value.trim() || !content.value.trim()) throw new Error('请填写标题和内容'); const value = { title: title.value.trim(), content: content.value.trim(), kind: kind.value, book_id: scope.value || null, enabled: true, tags: tags.value.split(/[,，]/).map(s => s.trim()).filter(Boolean) }; if (editing) await apiPatch(BASE + '/memories/' + editing.id, { ...value, revision: editing.revision }, signal); else await apiPost(BASE + '/memories', value, signal); await rerender(); }, signal, feedback, true);
    const list = card(area, '已保存记忆'); const data = await apiGet(BASE + '/memories', signal);
    if (!(data.memories || []).length) list.append(el('p', 'settings-help', '还没有记忆。'));
    for (const memory of data.memories || []) {
      const row = el('article', 'mai-connection'); row.append(el('strong', null, memory.title), el('small', null, (memory.book_id ? books.find(b => b.id === memory.book_id)?.title || '作品' : '全局') + ' · ' + (memory.enabled ? '已启用' : '已停用')), el('p', 'mai-result-text', memory.content)); const buttons = el('div', 'mai-actions'); row.append(buttons); const status = report(row);
      action(buttons, '编辑', async () => { editing = memory; title.value = memory.title; content.value = memory.content; kind.value = memory.kind; scope.value = memory.book_id || ''; tags.value = (memory.tags || []).join('，'); editor.scrollIntoView({ block: 'start' }); }, signal, status);
      action(buttons, memory.enabled ? '停用' : '启用', async () => { await apiPatch(BASE + '/memories/' + memory.id, { enabled: !memory.enabled, revision: memory.revision }, signal); await rerender(); }, signal, status);
      action(buttons, '删除', async () => { if (row.querySelector('.mai-confirm')) return; const confirm = el('div', 'mai-actions mai-confirm'); row.append(confirm); action(confirm, '确认删除记忆', async () => { await apiDelete(BASE + '/memories/' + memory.id + '?revision=' + memory.revision, signal); await rerender(); }, signal, status); }, signal, status); list.append(row);
    }
  }
  function search() {
    const root = card(area, '手机联网搜索', '手机直接请求配置的搜索服务，结果包含来源链接。'); const config = settings.search;
    const enabled = field(root, '启用联网搜索', config.enabled, 'checkbox'); const provider = select(root, '搜索服务', [['tavily', 'Tavily'], ['searxng', 'SearXNG'], ['brave', 'Brave']], config.provider);
    const address = field(root, '搜索 API 地址', config.base_url, 'url'), key = field(root, '搜索 API 密钥（保存后显示）', '', 'text'); key.autocomplete = 'off'; key.placeholder = config.key_configured ? '已保存密钥' : '';
    const count = number(root, '结果条数', config.max_results, 1, 10), timeout = number(root, '搜索超时（秒）', config.timeout, 3, 60); const tell = report(root);
    provider.addEventListener('change', () => { address.value = { tavily: 'https://api.tavily.com', brave: 'https://api.search.brave.com', searxng: '' }[provider.value]; });
    action(root, '保存搜索设置', async () => { await save({ search: { enabled: enabled.checked, provider: provider.value, base_url: address.value, api_key: key.value, max_results: Number(count.value), timeout: Number(timeout.value) } }); tell('搜索设置已保存'); }, signal, tell, true);
    const test = card(area, '搜索资料'), query = field(test, '搜索关键词', '', 'search'), results = el('div', 'mai-sources'), feedback = report(test);
    action(test, '开始搜索', async () => { feedback('正在搜索…'); const data = await apiPost(BASE + '/search', { query: query.value }, signal); results.replaceChildren(); for (const source of data.results || []) { let url; try { url = new URL(source.url); } catch { continue; } if (!['http:', 'https:'].includes(url.protocol)) continue; const link = el('a', null, source.title || url.hostname); link.href = url.href; link.target = '_blank'; link.rel = 'noopener noreferrer'; const row = el('article'); row.append(link, el('p', null, source.snippet || '')); results.append(row); } feedback('找到 ' + results.childElementCount + ' 条资料'); }, signal, feedback); test.append(results);
  }
}

export async function mountMobileAIStudio({ area, signal, bookId, chapter = 0, onSettings }) {
  styles(); area.classList.add('mobile-ai'); area.replaceChildren();
  const intro = card(area, '手机 AI 工作台', '可总结章节、整理人物、生成分镜或按描述生图。手机直接请求所选模型；结果保存在本机，重新打开可查看。'); const introTell = report(intro);
  action(intro, '设置模型与自动降级', async () => onSettings('models'), signal, introTell);
  const books = (await apiGet('/api/books', signal)).books.filter(b => b.format !== 'cbz'); if (signal.aborted) return;
  const book = select(intro, '参考作品', [['', '自由创作（不引用原文）'], ...books.map(b => [b.id, b.title])], bookId || '');
  const chapterSelect = select(intro, '参考章节', [], ''), sourceNote = el('p', 'settings-help'); intro.append(sourceNote); let version = 0;
  async function chapters() { const current = ++version; chapterSelect.replaceChildren(); if (!book.value) { chapterSelect.disabled = true; sourceNote.textContent = '自由创作不会发送书籍正文。'; return; } const data = await apiGet('/api/books/' + encodeURIComponent(book.value) + '/toc', signal); if (current !== version || signal.aborted) return; for (const ch of data.chapters) { const option = el('option', null, (ch.idx + 1) + '. ' + ch.title); option.value = ch.idx; chapterSelect.append(option); } chapterSelect.disabled = false; chapterSelect.value = String(Math.min(chapter, Math.max(0, data.chapters.length - 1))); sourceNote.textContent = '执行文本分析时将向所选模型发送本章原文（最多 24000 字符）及已启用的相关记忆。'; }
  book.addEventListener('change', () => { chapters().catch(e => introTell(e.message, true)); history().catch(e => introTell(e.message, true)); }); await chapters();
    const textCard = card(area, '文本分析与问答'); const task = select(textCard, '文本任务', [['summary', '章节总结'], ['characters', '人物与人格'], ['storyboard', '漫画分镜'], ['ask', '自由问答']], 'summary');
  const question = field(textCard, '问题或创作要求', '', 'textarea'); question.rows = 4; question.maxLength = 6000; question.placeholder = '例如：梳理人物关系；写一份六格漫画分镜；解释这一段。'; const textTell = report(textCard);
  const textResults = el('div', 'mai-text-results');
  action(textCard, '开始文本 AI', async () => {
    let context = ''; if (book.value) { const data = await apiGet('/api/books/' + encodeURIComponent(book.value) + '/chapters/' + chapterSelect.value, signal); context = data.paragraphs.join('\n').slice(0, 24000); }
    const instructions = { summary: '请总结本章情节、关键事件和伏笔。', characters: '请整理本章人物的身份、人格、关系和原文证据，未知信息写未知。', storyboard: '请根据原文写一份六格漫画分镜，逐格给出景别、画面、角色动作、对白和英文绘图提示词。', ask: '' };
    const prompt = [instructions[task.value], question.value.trim()].filter(Boolean).join('\n'); if (!prompt) throw new Error('请填写问题');
    textTell('正在请求文本模型，失败时会按模型池切换…'); await apiPost(BASE + '/text', { prompt, context, book_id: book.value || null, task: task.value }, signal); textTell('结果已保存到手机'); await history();
  }, signal, textTell, true); textCard.append(textResults);
  const imageCard = card(area, '生图与漫画画面', '按画面描述生成一张图片；也可把分镜结果用于绘图。生成的原图会下载保存到手机应用内。');
  const prompt = field(imageCard, '画面描述', '', 'textarea'); prompt.rows = 6; prompt.maxLength = 12000; prompt.placeholder = '描述人物、场景、动作、景别和画风。'; const imageTell = report(imageCard), images = el('div', 'mai-images');
  action(imageCard, '生成图片', async () => { if (!prompt.value.trim()) throw new Error('请填写画面描述'); imageTell('正在生图并保存原图…'); await apiPost(BASE + '/image', { prompt: prompt.value.trim(), book_id: book.value || null }, signal); imageTell('图片已保存到手机'); await history(); }, signal, imageTell, true); imageCard.append(images);
  async function history() {
    const data = await apiGet(BASE + '/history' + (book.value ? '?book_id=' + encodeURIComponent(book.value) : ''), signal); if (signal.aborted) return;
    textResults.replaceChildren(); images.replaceChildren();
    for (const result of (data.results || []).slice(0, 20)) { const row = el('article', 'mai-history'); row.append(el('small', null, result.model + ' · ' + fmtTime(result.created_at)), el('div', 'mai-result-text', result.text)); action(row, '用于绘图描述', async () => { prompt.value = result.text.slice(0, 12000); imageCard.scrollIntoView({ block: 'start' }); }, signal, textTell); textResults.append(row); }
    for (const item of (data.images || []).slice(0, 20)) { const row = el('figure', 'mai-history'); const img = el('img'); img.src = item.url; img.alt = item.prompt || 'AI 生成画面'; img.loading = 'lazy'; row.append(img, el('figcaption', 'settings-help', item.model + ' · ' + fmtTime(item.created_at)), el('p', 'mai-result-text', item.prompt)); images.append(row); }
  }
  await history();
}
