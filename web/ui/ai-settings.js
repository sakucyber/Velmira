import { el, apiGet, apiPost, apiPatch, apiDelete, debounce, fmtTime } from '../util/core.js';

const FEATURE_API = '/api/ai/features';
const MEMORY_API = '/api/ai/memories';
const KINDS = [['personality', '人格与性格'], ['meme', '梗与表达习惯'], ['world', '世界观'], ['note', '其他备注']];

function field(parent, label, value = '', type = 'text') {
  const wrap = el('label', 'ai-settings-field');
  const input = el(type === 'textarea' ? 'textarea' : 'input', 'ai-settings-input');
  input.setAttribute('aria-label', label);
  if (type !== 'textarea') input.type = type;
  if (type === 'checkbox') { wrap.classList.add('ai-settings-check'); input.checked = !!value; }
  else input.value = value ?? '';
  wrap.append(el('span', null, label), input); parent.append(wrap); return input;
}

function select(parent, label, options, value) {
  const wrap = el('label', 'ai-settings-field');
  const input = el('select', 'ai-settings-input'); input.setAttribute('aria-label', label);
  for (const [id, title] of options) { const option = el('option', null, title); option.value = id; input.append(option); }
  if (value != null) input.value = value;
  wrap.append(el('span', null, label), input); parent.append(wrap); return input;
}

function feedback(parent) {
  const node = el('p', 'ai-settings-feedback'); node.setAttribute('role', 'status'); node.setAttribute('aria-live', 'polite'); parent.append(node);
  return (message, error = false) => { node.textContent = message; node.dataset.error = String(error); };
}

function action(parent, title, task, signal, report, className = '') {
  const button = el('button', 'btn ' + className, title); button.type = 'button'; parent.append(button);
  button.addEventListener('click', async () => {
    if (button.disabled || signal.aborted) return;
    button.disabled = true;
    try { await task(); }
    catch (error) { if (!signal.aborted) report(error.status === 409 ? '设置已在其他页面变化，请重新加载后再保存。' : error.message || '操作失败', true); }
    finally { if (button.isConnected) button.disabled = false; }
  });
  return button;
}

function numeric(input, min, max, step = 1) {
  input.min = String(min); input.max = String(max); input.step = String(step); return input;
}

function title(area, text, help) {
  area.append(el('h3', 'settings-heading', text), el('p', 'settings-help', help));
}

function reloadAction(parent, context, report) {
  return action(parent, '重新加载设置', () => mountAISettings(context), context.signal, report);
}

export async function mountAISettings(context) {
  const { area, signal, section } = context;
  area.replaceChildren(el('p', 'settings-help', '正在加载设置…'));
  const response = await apiGet(FEATURE_API, signal);
  if (signal.aborted) return;
  area.replaceChildren(); area.classList.add('ai-settings');
  let settings = response.settings;
  let saveQueue = Promise.resolve();
  const save = patch => {
    // Serialize auto-saves so each request uses the revision returned by the
    // previous one. This also keeps rapid typing from losing the last edit.
    const run = saveQueue.then(async () => {
      const result = await apiPatch(FEATURE_API, { ...patch, revision: settings.revision }, signal);
      settings = result.settings; return settings;
    });
    saveQueue = run.catch(() => {});
    return run;
  };
  if (section === 'search') mountSearch(context, settings, save);
  else if (section === 'prompts') mountPrompts(context, settings, save);
  else if (section === 'memory') await mountMemories(context, settings, save);
}

export async function mountTranslationSettings({ area, signal }) {
  area.replaceChildren(el('p', 'settings-help', '正在加载翻译服务设置…'));
  const response = await apiGet('/api/translation/settings', signal);
  if (signal.aborted) return;
  const state = response.settings || {};
  area.replaceChildren(); area.classList.add('ai-settings');
  title(area, '翻译服务', '选中文本后点击“翻译”即可使用。设置会在修改后自动保存；Bilibili 默认选择速度较快的 Index-Translate-35B-A3B。');
  const form = el('div', 'ai-settings-card'); area.append(form);
  const provider = select(form, '翻译服务', [['ai', '文本 AI 模型池'], ['bilibili', 'Bilibili 翻译'], ['microsoft', 'Microsoft Translator'], ['custom', '自有 OpenAI 兼容 API']], state.provider || 'ai');
  const enabled = field(form, '启用独立翻译服务', !!state.enabled, 'checkbox');
  const base = field(form, 'API 地址', state.base_url || '', 'url'); base.maxLength = 500;
  const model = field(form, '模型名称', state.model || ''); model.maxLength = 200;
  const region = field(form, 'Microsoft 区域（可选）', state.region || ''); region.maxLength = 100;
  const regionWrap = region.closest('.ai-settings-field');
  const key = field(form, 'API 密钥', state.api_key || '', 'text'); key.autocomplete = 'off'; key.maxLength = 2000;
  const baseWrap = base.closest('.ai-settings-field');
  const modelWrap = model.closest('.ai-settings-field');
  const keyWrap = key.closest('.ai-settings-field');
  const modelBox = el('div', 'ai-settings-models'); form.append(modelBox);
  const listButton = el('button', 'btn', '获取厂商模型列表'); listButton.type = 'button'; modelBox.append(listButton);
  const models = el('select', 'ai-settings-input'); models.setAttribute('aria-label', '厂商模型列表'); modelBox.append(models);
  const modelHint = el('p', 'settings-help', '可从厂商列表选择模型；列表只用于选择，不会覆盖已保存的其他模型。'); modelBox.append(modelHint);
  const report = feedback(form);
  const defaults = { ai: ['', 'gpt-6-luna'], bilibili: ['https://index-translate.bilibili.com/v1', 'Index-Translate-35B-A3B'], microsoft: ['https://api.cognitive.microsofttranslator.com', ''], custom: ['', ''] };
  let previousProvider = provider.value;
  let savePromise = null, pendingDraft = null;
  const providerHelp = el('p', 'settings-help');
  const updateProviderHelp = () => {
    providerHelp.textContent = provider.value === 'bilibili'
      ? 'Bilibili 翻译可获取模型列表并自动选择速度较快的模型。'
      : provider.value === 'microsoft'
        ? 'Microsoft 使用区域字段和密钥；Microsoft 不提供 OpenAI 模型列表。'
        : provider.value === 'ai'
          ? '文本 AI 翻译复用当前文本模型池。'
          : '自有服务可使用任意 HTTP/HTTPS 地址和模型；密钥会直接保存在本机。';
  };
  const updateProviderVisibility = () => {
    const isPool = provider.value === 'ai';
    const isMicrosoft = provider.value === 'microsoft';
    enabled.closest('.ai-settings-field').hidden = isPool;
    baseWrap.hidden = isPool;
    modelWrap.hidden = isPool;
    keyWrap.hidden = isPool;
    regionWrap.hidden = !isMicrosoft;
    modelBox.hidden = isPool || isMicrosoft;
    providerHelp.hidden = false;
    updateProviderHelp();
  };
  const patch = () => {
    const value = { provider: provider.value, enabled: !!enabled.checked, base_url: base.value.trim(), model: model.value.trim(), region: region.value.trim() };
    const secret = key.value.trim();
    // An entered key is saved directly.  Empty values leave the stored key as
    // is, which keeps auto-save from clearing it while the user edits fields.
    if (secret) value.api_key = secret;
    return value;
  };
  const save = (message = '已自动保存') => {
    pendingDraft = patch();
    if (!savePromise) {
      const drain = async () => {
        while (pendingDraft && !signal.aborted) {
          const draft = pendingDraft; pendingDraft = null;
          try { await apiPatch('/api/translation/settings', draft, signal); }
          catch (error) { if (!pendingDraft) throw error; else continue; }
          if (signal.aborted) return;
          // Merge edits made during the request into one latest complete
          // draft. Requests stay serial so an old save cannot land last.
          const latest = patch();
          if (JSON.stringify(latest) !== JSON.stringify(draft)) {
            pendingDraft = latest; delayedSave.cancel();
          }
          if (!pendingDraft) report(message);
        }
      };
      savePromise = drain().finally(() => { savePromise = null; });
    }
    return savePromise;
  };
  const delayedSave = debounce(() => save().catch(error => { if (!signal.aborted) report(error.message || '自动保存失败', true); }), 450);
  signal.addEventListener('abort', () => delayedSave.cancel(), { once: true });
  const applyPreset = (saveAfter = false) => {
    const preset = defaults[provider.value] || defaults.ai;
    if (!base.value || base.value === defaults[previousProvider]?.[0]) base.value = preset[0];
    const legacyTranslationDefaults = new Set(['gpt-4o-mini', 'gpt-6-luna', 'gpt-oss-120b', 'translate']);
    if (!model.value || model.value === defaults[previousProvider]?.[1] ||
        provider.value === 'bilibili' && legacyTranslationDefaults.has(model.value.trim())) model.value = preset[1];
    regionWrap.hidden = provider.value !== 'microsoft';
    modelBox.hidden = provider.value === 'ai' || provider.value === 'microsoft';
    if (provider.value === 'ai') enabled.checked = false;
    previousProvider = provider.value;
    updateProviderVisibility();
    if (saveAfter) delayedSave();
  };
  provider.addEventListener('change', () => applyPreset(true));
  for (const input of [enabled, base, model, region, key]) {
    input.addEventListener(input.type === 'checkbox' ? 'change' : 'input', delayedSave);
  }
  applyPreset(false);
  listButton.addEventListener('click', async () => {
    if (listButton.disabled || signal.aborted) return;
    listButton.disabled = true; report('正在获取模型列表…');
    try {
      const result = await apiPost('/api/translation/models', { provider: provider.value, base_url: base.value.trim(), api_key: key.value.trim() }, signal);
      models.replaceChildren();
      for (const id of result.models || []) { const option = el('option', null, id); option.value = id; models.append(option); }
      if (result.recommended) {
        models.value = result.recommended;
        if (!model.value.trim() || provider.value === 'bilibili') { model.value = result.recommended; delayedSave(); }
      }
      models.onchange = () => { if (models.value) { model.value = models.value; delayedSave(); } };
      report((result.models || []).length ? `已获取 ${result.models.length} 个模型` : '厂商未返回模型');
    } catch (error) { report(error.message || '获取模型列表失败', true); }
    finally { if (listButton.isConnected) listButton.disabled = false; }
  });
  form.append(providerHelp, el('p', 'settings-help', '翻译结果只在当前弹窗显示，不写入书籍或长期记忆。'));
}

function mountSearch(context, settings, save) {
  const { area, signal } = context;
  title(area, '联网搜索', '通过你配置的搜索服务补充外部资料，结果附来源链接。搜索结果只作为临时参考，不会自动保存为长期记忆。');
  const form = el('div', 'ai-settings-card'); area.append(form);
  const enabled = field(form, '启用联网搜索', settings.search.enabled, 'checkbox');
  const provider = select(form, '搜索服务', [['tavily', 'Tavily'], ['searxng', 'SearXNG'], ['brave', 'Brave Search']], settings.search.provider);
  const base = field(form, '搜索 API 地址', settings.search.base_url, 'url'); base.maxLength = 500;
  const key = field(form, '搜索 API 密钥（保存后显示）', settings.search.api_key || '', 'text'); key.autocomplete = 'off';
  key.placeholder = settings.search.key_configured ? '已保存密钥' : '未配置；SearXNG 可按服务要求留空'; key.maxLength = 2000;
  const row = el('div', 'ai-settings-grid'); form.append(row);
  const count = numeric(field(row, '搜索结果条数', settings.search.max_results, 'number'), 1, 10);
  const timeout = numeric(field(row, '搜索超时（秒）', settings.search.timeout, 'number'), 3, 60);
  const auto = field(form, '自动为漫画场景检索资料', settings.search.auto_enrich, 'checkbox');
  form.append(el('p', 'settings-help', '开启自动检索后，会向所选搜索服务发送当前场景的检索词；不会发送完整小说。请按需要开启。'));
  const report = feedback(form);
  const searchPatch = () => {
    const search = { enabled: enabled.checked, provider: provider.value, base_url: base.value.trim(), auto_enrich: auto.checked,
      max_results: Number(count.value), timeout: Number(timeout.value) };
    if (key.value.trim()) search.api_key = key.value.trim();
    return { search };
  };
  let saving = false, queued = false;
  const saveSearch = async () => {
    if (signal.aborted) return;
    if (saving) { queued = true; return; }
    saving = true;
    try {
      const saved = await save(searchPatch());
      key.placeholder = saved.search.key_configured ? '已保存密钥' : '未配置密钥';
      report('联网搜索设置已自动保存');
    } catch (error) { if (!signal.aborted) report(error.message || '自动保存失败', true); }
    finally { saving = false; if (queued && !signal.aborted) { queued = false; delayedSearchSave(); } }
  };
  const delayedSearchSave = debounce(saveSearch, 450);
  for (const input of [enabled, provider, base, key, count, timeout, auto]) {
    input.addEventListener(input.type === 'checkbox' || input.tagName === 'SELECT' ? 'change' : 'input', delayedSearchSave);
  }
  const actions = el('div', 'ai-settings-actions'); form.append(actions);
  action(actions, '立即保存', saveSearch, signal, report, 'btn-primary');
  reloadAction(actions, context, report);
  const defaults = { tavily: 'https://api.tavily.com', searxng: '', brave: 'https://api.search.brave.com' };
  let previousProvider = provider.value;
  provider.addEventListener('change', () => {
    if (!base.value || base.value === defaults[previousProvider]) base.value = defaults[provider.value];
    previousProvider = provider.value;
    delayedSearchSave();
  });
  const test = el('div', 'ai-settings-card'); area.append(test);
  test.append(el('h4', null, '测试搜索'));
  const query = field(test, '搜索关键词', '', 'search'); query.maxLength = 300;
  test.append(el('p', 'settings-help', '先保存搜索配置，再测试。测试会访问所选搜索服务。'));
  const results = el('div', 'ai-search-results'); results.setAttribute('aria-label', '联网搜索结果');
  const testReport = feedback(test);
  action(test, '测试联网搜索', async () => {
    if (!query.value.trim()) throw new Error('请填写搜索关键词');
    testReport('正在搜索…');
    const result = await apiPost('/api/ai/web-search', { query: query.value.trim() }, signal);
    if (signal.aborted) return;
    results.replaceChildren();
    for (const source of result.results || []) {
      const card = el('article', 'ai-search-source');
      let url;
      try { url = new URL(source.url); } catch { continue; }
      if (!['https:', 'http:'].includes(url.protocol)) continue;
      const link = el('a', null, source.title || url.hostname); link.href = url.href; link.target = '_blank'; link.rel = 'noopener noreferrer';
      card.append(link, el('small', null, url.hostname), el('p', null, source.snippet || '')); results.append(card);
    }
    testReport(results.childElementCount ? '已找到 ' + results.childElementCount + ' 条资料' : '没有找到可展示的资料');
  }, signal, testReport);
  test.append(results);
}

function mountPrompts(context, settings, save) {
  const { area, signal } = context;
  title(area, '提示词', '这些内容会作为创作偏好补充。默认系统的结构、原文依据和人物身份约束保持固定。');
  const form = el('div', 'ai-settings-card'); area.append(form);
  const text = field(form, '文本创作补充', settings.prompts.text, 'textarea'); text.maxLength = 6000; text.rows = 7;
  text.placeholder = '例如：对白简洁，保留原文幽默，优先描绘角色之间的互动。';
  const image = field(form, '生图正向补充', settings.prompts.image, 'textarea'); image.maxLength = 3000; image.rows = 5;
  image.placeholder = '例如：柔和光线、清晰线稿、适度留白。';
  const negative = field(form, '生图负向补充', settings.prompts.negative, 'textarea'); negative.maxLength = 2000; negative.rows = 4;
  negative.placeholder = '例如：水印、多余文字、错误手指。';
  const report = feedback(form);
  const promptPatch = () => ({ prompts: { text: text.value, image: image.value, negative: negative.value } });
  let saving = false, queued = false;
  const savePrompts = async () => {
    if (signal.aborted) return;
    if (saving) { queued = true; return; }
    saving = true;
    try { await save(promptPatch()); report('提示词已自动保存'); }
    catch (error) { if (!signal.aborted) report(error.message || '自动保存失败', true); }
    finally { saving = false; if (queued && !signal.aborted) { queued = false; delayedPromptSave(); } }
  };
  const delayedPromptSave = debounce(savePrompts, 450);
  for (const input of [text, image, negative]) input.addEventListener('input', delayedPromptSave);
  const buttons = el('div', 'ai-settings-actions'); form.append(buttons);
  action(buttons, '立即保存', savePrompts, signal, report, 'btn-primary');
  action(buttons, '清空自定义提示词', async () => {
    await save({ prompts: { text: '', image: '', negative: '' } }); text.value = image.value = negative.value = ''; report('已恢复默认提示词');
  }, signal, report);
  reloadAction(buttons, context, report);
}

async function mountMemories(context, settings, save) {
  const { area, signal, bookId } = context;
  title(area, '长期记忆', '手动保存人物性格、常用梗和世界观，供后续创作检索。可以作用于全部作品、某本书或某位角色。联网搜索结果不会自动写入这里。');
  const config = el('div', 'ai-settings-card'); area.append(config);
  const enabled = field(config, '启用长期记忆', settings.memory.enabled, 'checkbox');
  const grid = el('div', 'ai-settings-grid'); config.append(grid);
  const maxItems = numeric(field(grid, '每次最多引用记忆条数', settings.memory.max_items, 'number'), 1, 30);
  const maxChars = numeric(field(grid, '每次记忆字数上限', settings.memory.max_chars, 'number'), 500, 16000);
  config.append(el('p', 'settings-help', '关闭后仍可管理已保存记忆，但创作时不会引用。'));
  const configReport = feedback(config);
  const memoryPatch = () => ({ memory: { enabled: enabled.checked, max_items: Number(maxItems.value), max_chars: Number(maxChars.value) } });
  let memorySaving = false, memoryQueued = false;
  const saveMemory = async () => {
    if (signal.aborted) return;
    if (memorySaving) { memoryQueued = true; return; }
    memorySaving = true;
    try { await save(memoryPatch()); configReport('长期记忆设置已自动保存'); }
    catch (error) { if (!signal.aborted) configReport(error.message || '自动保存失败', true); }
    finally { memorySaving = false; if (memoryQueued && !signal.aborted) { memoryQueued = false; delayedMemorySave(); } }
  };
  const delayedMemorySave = debounce(saveMemory, 400);
  for (const input of [enabled, maxItems, maxChars]) input.addEventListener(input.type === 'checkbox' ? 'change' : 'input', delayedMemorySave);
  const configButtons = el('div', 'ai-settings-actions'); config.append(configButtons);
  action(configButtons, '立即保存', saveMemory, signal, configReport, 'btn-primary');
  reloadAction(configButtons, context, configReport);
  const metadata = await apiGet(MEMORY_API, signal);
  if (signal.aborted) return;
  const books = metadata.books || [], characters = metadata.characters || [];
  const bookTitle = id => books.find(book => book.id === id)?.title || '未命名作品';
  const scopes = [['global', '全局：全部作品'], ...books.map(book => ['book:' + book.id, '作品：' + book.title]),
    ...characters.map(character => ['character:' + character.id, '角色：' + character.name + ' · ' + bookTitle(character.book_id)])];
  const scopeValue = item => item.character_id ? 'character:' + item.character_id : item.book_id ? 'book:' + item.book_id : 'global';
  const scopeLabel = item => scopes.find(([id]) => id === scopeValue(item))?.[1] || '关联对象已移除';
  const editor = el('form', 'ai-settings-card ai-memory-editor'); editor.setAttribute('aria-label', '长期记忆编辑器'); area.append(editor);
  const editorHeading = el('h4', null, '新增记忆'); editor.append(editorHeading);
  const name = field(editor, '记忆标题'); name.maxLength = 120; name.required = true;
  const kind = select(editor, '记忆类型', KINDS, 'personality');
  const scope = select(editor, '记忆作用范围', scopes, books.some(book => book.id === bookId) ? 'book:' + bookId : 'global');
  const content = field(editor, '记忆内容', '', 'textarea'); content.rows = 6; content.maxLength = 10000; content.required = true;
  const tags = field(editor, '记忆标签（逗号分隔）'); tags.maxLength = 1020; tags.placeholder = '例如：林澄，冷幽默，性格';
  const active = field(editor, '此条记忆参与创作', true, 'checkbox');
  const editorButtons = el('div', 'ai-settings-actions'); editor.append(editorButtons);
  const editorReport = feedback(editor);
  let editing = null;
  const reset = () => {
    editing = null; editorHeading.textContent = '新增记忆'; name.value = content.value = tags.value = ''; kind.value = 'personality';
    scope.value = books.some(book => book.id === bookId) ? 'book:' + bookId : 'global'; active.checked = true;
    saveButton.textContent = '保存新记忆'; editorReport('');
  };
  const saveEntry = async ({ auto = false } = {}) => {
    if (!editor.reportValidity()) return;
    const selected = scope.value, character = selected.startsWith('character:') ? characters.find(item => item.id === selected.slice(10)) : null;
    const value = { title: name.value.trim(), kind: kind.value, content: content.value.trim(), tags: [...new Set(tags.value.split(/[,，]/).map(tag => tag.trim()).filter(Boolean))],
      book_id: character ? character.book_id : selected.startsWith('book:') ? selected.slice(5) : null,
      character_id: character?.id || null, enabled: active.checked };
    if (value.tags.length > 20 || value.tags.some(tag => tag.length > 50)) throw new Error('最多填写 20 个标签，每个不超过 50 字符');
    const result = editing
      ? await apiPatch(MEMORY_API + '/' + encodeURIComponent(editing.id), { ...value, revision: editing.revision }, signal)
      : await apiPost(MEMORY_API, value, signal);
    if (signal.aborted) return;
    if (auto) {
      editing = result.memory || { ...(editing || {}), ...value, revision: (editing?.revision || 0) + 1 };
      editorHeading.textContent = '编辑记忆'; saveButton.textContent = '立即保存记忆修改';
      editorReport('记忆已自动保存'); await load(); return;
    }
    reset(); editorReport('记忆已保存'); await load();
  };
  const saveButton = action(editorButtons, '立即保存记忆', saveEntry, signal, editorReport, 'btn-primary');
  editor.addEventListener('submit', event => { event.preventDefault(); saveButton.click(); });
  action(editorButtons, '取消编辑 / 新增记忆', async () => reset(), signal, editorReport);
  // Existing records and complete new entries are persisted as the user edits.
  // Keep the editor mounted so typing is never interrupted by a list reload.
  const delayedEntrySave = debounce(() => {
    if (name.value.trim() && content.value.trim() && editor.reportValidity()) {
      saveEntry({ auto: true }).catch(error => { if (!signal.aborted) editorReport(error.message || '记忆自动保存失败', true); });
    }
  }, 700);
  for (const input of [name, kind, scope, content, tags, active]) input.addEventListener(input.type === 'checkbox' || input.tagName === 'SELECT' ? 'change' : 'input', delayedEntrySave);
  signal.addEventListener('abort', () => delayedEntrySave.cancel(), { once: true });
  const filters = el('div', 'ai-settings-card ai-memory-filters'); area.append(filters);
  const query = field(filters, '搜索长期记忆', '', 'search'); query.maxLength = 200;
  const filterScope = select(filters, '筛选记忆范围', [['all', '全部范围'], ...scopes], 'all');
  const filterKind = select(filters, '筛选记忆类型', [['all', '全部类型'], ...KINDS], 'all');
  const list = el('div', 'ai-memory-list'); list.setAttribute('aria-label', '长期记忆列表'); area.append(list);
  const listReport = feedback(filters);
  let requestVersion = 0;
  const edit = item => {
    editing = item; editorHeading.textContent = '编辑记忆'; name.value = item.title; kind.value = item.kind;
    scope.value = scopeValue(item); content.value = item.content; tags.value = (item.tags || []).join('，'); active.checked = item.enabled;
    saveButton.textContent = '保存记忆修改'; editorReport(''); editor.scrollIntoView({ block: 'start', behavior: 'smooth' }); name.focus({ preventScroll: true });
  };
  const render = memories => {
    list.replaceChildren();
    const shown = memories.filter(item => (filterScope.value === 'all' || scopeValue(item) === filterScope.value) && (filterKind.value === 'all' || item.kind === filterKind.value));
    listReport('共 ' + shown.length + ' 条记忆');
    if (!shown.length) list.append(el('p', 'settings-help', '还没有符合条件的记忆。可以在上方新增。'));
    for (const item of shown) {
      const card = el('article', 'ai-settings-card ai-memory-card'); card.dataset.memoryId = item.id;
      card.append(el('h4', null, item.title), el('small', 'ai-memory-meta', scopeLabel(item) + ' · ' + (KINDS.find(([id]) => id === item.kind)?.[1] || item.kind) + ' · ' + (item.enabled ? '已启用' : '已停用')),
        el('p', 'ai-memory-content', item.content));
      if (item.tags?.length) card.append(el('p', 'ai-memory-tags', item.tags.map(tag => '#' + tag).join(' ')));
      card.append(el('small', 'ai-memory-meta', '版本 ' + item.revision + ' · ' + fmtTime(item.updated_at)));
      const buttons = el('div', 'ai-settings-actions'); card.append(buttons);
      action(buttons, '编辑', async () => edit(item), signal, listReport).setAttribute('aria-label', '编辑记忆 ' + item.title);
      action(buttons, item.enabled ? '停用' : '启用', async () => {
        await apiPatch(MEMORY_API + '/' + encodeURIComponent(item.id), { enabled: !item.enabled, revision: item.revision }, signal);
        await load();
      }, signal, listReport).setAttribute('aria-label', (item.enabled ? '停用记忆 ' : '启用记忆 ') + item.title);
      action(buttons, '删除', async () => {
        if (card.querySelector('.ai-memory-delete-confirm')) return;
        const confirmation = el('div', 'ai-memory-delete-confirm'); confirmation.append(el('span', null, '删除“' + item.title + '”？'));
        action(confirmation, '确认删除', async () => {
          await apiDelete(MEMORY_API + '/' + encodeURIComponent(item.id) + '?revision=' + item.revision, signal);
          if (editing?.id === item.id) reset(); await load();
        }, signal, listReport, 'btn-danger').setAttribute('aria-label', '确认删除记忆 ' + item.title);
        action(confirmation, '保留', async () => confirmation.remove(), signal, listReport); card.append(confirmation);
      }, signal, listReport, 'btn-danger').setAttribute('aria-label', '删除记忆 ' + item.title);
      list.append(card);
    }
  };
  async function load() {
    const version = ++requestVersion;
    try {
      const data = await apiGet(MEMORY_API + (query.value.trim() ? '?q=' + encodeURIComponent(query.value.trim()) : ''), signal);
      if (signal.aborted || version !== requestVersion) return;
      render(data.memories || []);
    } catch (error) { if (!signal.aborted && version === requestVersion) listReport(error.message || '记忆读取失败', true); }
  }
  const search = debounce(load, 250); query.addEventListener('input', search);
  filterScope.addEventListener('change', load); filterKind.addEventListener('change', load);
  signal.addEventListener('abort', () => search.cancel(), { once: true });
  render(metadata.memories || []);
}
