import { el, apiGet, apiPost, apiPatch, debounce } from '../util/core.js';

const STRATEGIES = [
  ['ordered', '按顺序', '优先使用第 1 个可用模型，失败或超时后依次切换。'],
  ['round_robin', '轮流', '每次请求轮流分配起始模型，失败后继续尝试其他模型。'],
  ['random', '随机', '每次请求随机选择起始模型，失败后继续尝试其他模型。'],
];
const REASONING = /(?:^|[\/_.-])(?:o[134](?:$|[-_.])|reasoner|reasoning|thinking|deepseek-r|qwq|gpt-oss)/i;
const EFFORTS = [['', '不指定（不下发）'], ['minimal', 'minimal'], ['low', 'low'], ['medium', 'medium'], ['high', 'high'], ['max', 'max']];

/** Global model pools. Credentials stay in server-side connection profiles. */
export async function mountModelRouting({ area, kind, signal }) {
  if (!document.getElementById('model-library-styles')) {
    const link = el('link'); link.id = 'model-library-styles'; link.rel = 'stylesheet'; link.href = new URL('../styles/model-library.css', import.meta.url).href; document.head.appendChild(link);
  }
  const name = kind === 'text' ? '文本' : '生图';
  const root = el('section', 'model-routing');
  root.dataset.routing = kind;
  area.appendChild(root);
  const heading = el('div', 'routing-heading');
  heading.appendChild(el('h4', null, name + '模型池 · 降级与负载均衡'));
  root.appendChild(heading);
  const loading = el('p', 'manga-note', '正在读取模型池…'); root.appendChild(loading);
  const active = () => !signal.aborted && root.isConnected;
  // Assigned after the parameter controls exist; renderModels may run earlier.
  let updateTokenHint = () => {};
  let connections, config, status, presets = [];
  try {
    const [pool, saved] = await Promise.all([apiGet('/api/ai/routing/' + kind, signal), apiGet('/api/ai/connections', signal)]);
    if (!active()) return;
    config = pool.config; status = pool.status; presets = pool.presets || [];
    connections = saved.connections.filter(c => c.kind === kind);
  } catch (err) {
    if (active()) loading.textContent = '模型池加载失败：' + err.message;
    return;
  }
  loading.remove();
  let models = config.models.map(m => ({ ...m })), strategy = config.strategy, pending = false;
  const same = (a, b) => a.connection_id === b.connection_id && a.model === b.model;
  const connectionFor = member => connections.find(c => c.id === member.connection_id);
  const field = (parent, label, value, type = 'text') => {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, label));
    const input = el(type === 'textarea' ? 'textarea' : 'input');
    if (type !== 'textarea') input.type = type;
    if (type === 'checkbox') input.checked = !!value; else input.value = value ?? '';
    input.setAttribute('aria-label', name + '模型池：' + label);
    wrap.appendChild(input); parent.appendChild(wrap); return input;
  };
  const button = (parent, label, run, cls = '') => {
    const node = el('button', 'btn ' + cls, label); node.type = 'button'; parent.appendChild(node);
    node.addEventListener('click', async () => {
      if (pending) return;
      pending = true; root.setAttribute('aria-busy', 'true'); root.inert = true;
      try { await run(); }
      catch (err) { if (active()) { feedback.textContent = err.message || '操作失败'; feedback.dataset.error = 'true'; } }
      finally {
        pending = false;
        root.inert = false; root.removeAttribute('aria-busy');
      }
    });
    return node;
  };
  const enabled = field(heading, '启用模型池', config.enabled, 'checkbox');
  enabled.parentElement.classList.add('routing-enable');
  root.appendChild(el('p', 'manga-note', kind === 'text'
    ? '适用于本机所有书籍的文本请求。请添加模型并启用模型池，同时启用上下文 AI。'
    : '适用于本机所有书籍的云端生图请求。开启后使用下面的模型池；ComfyUI 和占位图继续使用各书设置。所有备用生图模型都应支持当前尺寸与参考图。'));
  root.appendChild(el('h5', 'routing-section-title', '模型与优先顺序'));
  const list = el('ol', 'routing-models'); root.appendChild(list);
  list.setAttribute('aria-label', name + '模型优先顺序');
  const addRow = el('div', 'routing-add'); root.appendChild(addRow);
  const choice = el('select'); choice.setAttribute('aria-label', name + '模型池：添加模型'); addRow.appendChild(choice);
  let choices = [];
  const feedback = el('p', 'manga-note routing-feedback'); feedback.setAttribute('role', 'status');
  let saveTimer = 0, saving = false, saveQueued = false;
  let editVersion = 0, savedVersion = 0, conflicted = false, refreshVersion = 0, refreshing = false;
  const markConflict = () => {
    conflicted = true; saveQueued = false; clearTimeout(saveTimer); saveTimer = 0;
    feedback.textContent = '模型池已在其他页面变化，本地草稿已保留并暂停自动保存。点击“重新加载模型池”可放弃草稿并读取最新设置。';
    feedback.dataset.error = 'true';
  };
  const saveNow = async () => {
    if (!active()) return;
    if (conflicted) { markConflict(); return; }
    if (refreshing) { feedback.textContent = '正在同步模型池，请稍候再保存'; return; }
    if (saving) { saveQueued = true; return; }
    saveQueued = false; clearTimeout(saveTimer); saveTimer = 0;
    if (enabled.checked && !models.length) {
      feedback.textContent = '启用模型池前请至少添加一个模型'; feedback.dataset.error = 'true';
      return;
    }
    const version = editVersion;
    const patch = { enabled: enabled.checked, strategy, models: models.map(model => ({ ...model })), preset: preset.value, timeout: Number(timeout.value), cooldown: Number(cooldown.value), revision: config.revision };
    if (temperature) patch.temperature = Number(temperature.value);
    if (maxTokens) patch.max_tokens = Number(maxTokens.value);
    saving = true; feedback.textContent = '正在自动保存…'; delete feedback.dataset.error;
    try {
      const result = await apiPatch('/api/ai/routing/' + kind, patch, signal);
      if (!active()) return;
      config = result.config; status = result.status;
      if (version !== editVersion) { markConflict(); }
      else { savedVersion = version; }
      if (conflicted) markConflict();
      else { feedback.textContent = name + '模型池已自动保存' + (config.enabled ? '并启用' : '，添加模型后请启用模型池'); delete feedback.dataset.error; }
      renderStatus();
    } catch (err) {
      if (active()) {
        if (err.status === 409 || conflicted) markConflict();
        else { feedback.textContent = err.message || '自动保存失败'; feedback.dataset.error = 'true'; }
      }
    } finally {
      saving = false;
      if (saveQueued && !conflicted && active()) { saveQueued = false; clearTimeout(saveTimer); saveTimer = setTimeout(saveNow, 50); }
    }
  };
  const scheduleSave = (message = '修改已记录，正在自动保存…') => {
    editVersion += 1;
    if (conflicted || refreshing) { markConflict(); return; }
    feedback.textContent = message; delete feedback.dataset.error;
    clearTimeout(saveTimer); saveTimer = setTimeout(saveNow, 650);
  };
  const dirty = scheduleSave;
  let dragging;
  const stopDrag = (commit = false) => {
    if (!dragging) return;
    const current = dragging; dragging = null;
    try { current.handle.releasePointerCapture(current.pointerId); } catch {}
    if (commit) {
      const [moved] = models.splice(current.from, 1); models.splice(current.target, 0, moved);
    }
    renderModels(); updateChoices();
    if (commit) dirty('已调整模型顺序，正在自动保存…');
    else feedback.textContent = '已取消拖动';
  };
  signal.addEventListener('abort', () => { if (dragging) stopDrag(false); }, { once: true });
  const updateChoices = () => {
    choices = connections.flatMap(c => c.models.map(model => ({ connection_id: c.id, model }))).filter(m => !models.some(existing => same(existing, m)));
    choice.replaceChildren();
    const prompt = el('option', null, choices.length ? '选择连接与模型…' : '请先保存连接，或所有模型已添加'); prompt.value = ''; choice.appendChild(prompt);
    choices.forEach((m, i) => { const option = el('option', null, connectionFor(m).label + ' · ' + m.model); option.value = String(i); choice.appendChild(option); });
    choice.disabled = !choices.length;
  };
  const renderModels = () => {
    list.replaceChildren();
    if (!models.length) { const empty = el('li', 'routing-empty', '还没有模型。添加后可设置优先级与切换方式。'); list.appendChild(empty); }
    models.forEach((member, index) => {
      const row = el('li', 'routing-model'); row.dataset.model = member.model; row.dataset.connectionId = member.connection_id;
      const handle = el('button', 'routing-drag-handle'); handle.type = 'button';
      handle.setAttribute('aria-label', name + '模型池：拖动第 ' + (index + 1) + ' 个模型'); handle.title = '拖动调整顺序，也可使用上下按钮';
      handle.appendChild(el('span', 'routing-grip', '⠿')); handle.appendChild(el('span', 'routing-number', String(index + 1))); row.appendChild(handle);
      handle.addEventListener('pointerdown', event => {
        if (pending || dragging || !event.isPrimary || event.button !== 0) return;
        event.preventDefault(); dragging = { handle, pointerId: event.pointerId, from: index, target: index }; handle.setPointerCapture(event.pointerId);
        row.classList.add('routing-dragging'); feedback.textContent = '正在拖动第 ' + (index + 1) + ' 个模型，松开调整顺序；将自动保存';
      });
      handle.addEventListener('pointermove', event => {
        if (!dragging || dragging.handle !== handle || event.pointerId !== dragging.pointerId) return;
        event.preventDefault();
        const rows = [...list.querySelectorAll('.routing-model')];
        let closest = index, distance = Infinity;
        rows.forEach((other, at) => {
          const rect = other.getBoundingClientRect(), offset = Math.abs(event.clientY - (rect.top + rect.height / 2));
          if (offset < distance) { closest = at; distance = offset; }
          other.classList.remove('routing-drop-target');
        });
        dragging.target = closest;
        if (closest !== index) rows[closest].classList.add('routing-drop-target');
        const bounds = list.getBoundingClientRect();
        if (event.clientY < bounds.top + 30 || event.clientY > bounds.bottom - 30) row.scrollIntoView({ block: 'nearest' });
      });
      handle.addEventListener('pointerup', event => { if (dragging?.handle === handle && event.pointerId === dragging.pointerId) stopDrag(true); });
      handle.addEventListener('pointercancel', () => { if (dragging?.handle === handle) stopDrag(false); });
      handle.addEventListener('keydown', event => {
        if (event.key === 'Escape' && dragging?.handle === handle) { event.preventDefault(); stopDrag(false); }
        if (!pending && ['ArrowUp', 'ArrowDown'].includes(event.key)) {
          event.preventDefault(); const target = index + (event.key === 'ArrowUp' ? -1 : 1);
          if (target < 0 || target >= models.length) return;
          [models[index], models[target]] = [models[target], models[index]]; renderModels(); updateChoices();
          list.querySelectorAll('.routing-drag-handle')[target]?.focus(); dirty('已将模型移至第 ' + (target + 1) + ' 位，正在自动保存…');
        }
      });
      const info = el('div', 'routing-model-info');
      const title = el('strong', null, member.model); title.title = member.model; info.appendChild(title);
      const connection = connectionFor(member);
      info.appendChild(el('small', null, connection ? connection.label + ' · 并发 ' + connection.max_concurrency + ' · ' + (connection.rpm ? connection.rpm + ' RPM' : '不限 RPM') : '连接已移除，请删除或重新添加'));
      row.appendChild(info);
      const actions = el('div', 'routing-model-actions'); row.appendChild(actions);
      const move = delta => { [models[index], models[index + delta]] = [models[index + delta], models[index]]; renderModels(); updateChoices(); dirty(); };
      const up = button(actions, '↑', () => move(-1), 'routing-icon'); up.setAttribute('aria-label', name + '模型池：上移第 ' + (index + 1) + ' 个模型'); up.disabled = index === 0;
      const down = button(actions, '↓', () => move(1), 'routing-icon'); down.setAttribute('aria-label', name + '模型池：下移第 ' + (index + 1) + ' 个模型'); down.disabled = index === models.length - 1;
      const remove = button(actions, '×', () => { models.splice(index, 1); renderModels(); updateChoices(); dirty(); }, 'routing-icon routing-remove'); remove.setAttribute('aria-label', name + '模型池：移除第 ' + (index + 1) + ' 个模型');
      list.appendChild(row);
    });
    updateTokenHint();
  };

  // A model-library edit can change both the available connections and the
  // remote pool revision. Refresh them together only while this editor is
  // clean; a local draft must never be silently replaced by a remote snapshot.
  const refreshPool = async (discardDraft = false) => {
    if (!active() || refreshing) return;
    if (!discardDraft && (saving || conflicted || editVersion !== savedVersion)) {
      markConflict();
      return;
    }
    if (discardDraft && saving) {
      feedback.textContent = '当前保存正在进行，请稍候再重新加载模型池'; feedback.dataset.error = 'true';
      return;
    }
    if (discardDraft) {
      conflicted = false; saveQueued = false; clearTimeout(saveTimer); saveTimer = 0;
    }
    const token = ++refreshVersion;
    refreshing = true;
    feedback.textContent = '正在同步模型池…'; delete feedback.dataset.error;
    try {
      const [pool, saved] = await Promise.all([
        apiGet('/api/ai/routing/' + kind, signal),
        apiGet('/api/ai/connections', signal),
      ]);
      if (!active() || token !== refreshVersion) return;
      if (saving || conflicted || editVersion !== savedVersion) {
        markConflict();
        return;
      }
      config = pool.config; status = pool.status; presets = pool.presets || presets;
      connections = saved.connections.filter(c => c.kind === kind);
      models = config.models.map(model => ({ ...model }));
      strategy = config.strategy; enabled.checked = config.enabled;
      if (preset) preset.value = config.preset || 'balanced';
      if (timeout) timeout.value = config.timeout;
      if (cooldown) cooldown.value = config.cooldown;
      if (temperature) temperature.value = config.temperature;
      if (maxTokens) maxTokens.value = config.max_tokens;
      savedVersion = editVersion;
      renderModels(); updateChoices(); describeStrategy(); renderStatus();
      feedback.textContent = '模型池已重新加载'; delete feedback.dataset.error;
    } catch (err) {
      if (active() && token === refreshVersion) { feedback.textContent = err.message || '模型池同步失败'; feedback.dataset.error = 'true'; }
    } finally {
      if (token === refreshVersion) refreshing = false;
    }
  };
  button(addRow, '添加模型', () => {
    if (choice.value === '') throw new Error('请先选择要添加的模型');
    if (models.length >= 20) throw new Error('每个模型池最多添加 20 个模型');
    models.push({ ...choices[Number(choice.value)] }); renderModels(); updateChoices(); dirty();
  }).setAttribute('aria-label', name + '模型池：添加所选模型');
  renderModels(); updateChoices();

  const newConnection = el('details', 'routing-new-connection');
  newConnection.appendChild(el('summary', null, '＋ 保存新的' + name + '连接'));
  newConnection.appendChild(el('p', 'manga-note', '填写后会自动保存，API 密钥直接保存在本机配置中。保存后可加入模型池。'));
  root.appendChild(newConnection);
  const connectionLabel = field(newConnection, '连接名称', '');
  const address = field(newConnection, 'API 地址（包含 /v1 等接口前缀）', ''); address.placeholder = 'http://localhost:11434/v1 或 https://example.com/v1';
  const key = field(newConnection, 'API 密钥', '', 'password'); key.autocomplete = 'new-password';
  const modelNames = field(newConnection, '模型名称（多个用逗号或换行分隔）', '', 'textarea'); modelNames.rows = 2;
  const connectionNumbers = el('div', 'routing-parameters'); newConnection.appendChild(connectionNumbers);
  const rpm = field(connectionNumbers, '每分钟请求上限（0 为不限）', 0, 'number'); rpm.min = '0'; rpm.max = '1000'; rpm.step = '1';
  const concurrency = field(connectionNumbers, '最大并发', 1, 'number'); concurrency.min = '1'; concurrency.max = '8'; concurrency.step = '1';
  let jsonMode, referenceApi, reasoningEffort;
  if (kind === 'text') {
    jsonMode = field(newConnection, '要求 JSON 输出（服务需支持）', true, 'checkbox');
    const effortWrap = el('label', 'manga-field');
    effortWrap.appendChild(el('span', null, '推理强度（可选，未选择则不下发）'));
    reasoningEffort = el('select'); reasoningEffort.setAttribute('aria-label', name + '模型池：推理强度');
    for (const [value, label] of EFFORTS) { const option = el('option', null, label); option.value = value; reasoningEffort.appendChild(option); }
    effortWrap.appendChild(reasoningEffort); newConnection.appendChild(effortWrap);
  } else {
    const wrap = el('label', 'manga-field'); wrap.appendChild(el('span', null, '参考图接口'));
    referenceApi = el('select'); referenceApi.setAttribute('aria-label', name + '模型池：新连接参考图接口');
    for (const [value, label] of [['edits', '标准图像编辑接口'], ['generations', 'Agnes JSON 参考图接口']]) { const option = el('option', null, label); option.value = value; referenceApi.appendChild(option); }
    wrap.appendChild(referenceApi); newConnection.appendChild(wrap);
  }
  const connectionStatus = el('p', 'manga-note routing-autosave'); connectionStatus.setAttribute('role', 'status'); newConnection.appendChild(connectionStatus);
  let savedConnectionId = '';
  const saveConnection = async () => {
    const names = [...new Set(modelNames.value.split(/[,，\n]/).map(v => v.trim()).filter(Boolean))];
    if (!connectionLabel.value.trim() || !address.value.trim()) throw new Error('请填写连接名称和 API 地址');
    const payload = { label: connectionLabel.value.trim(), kind, models: names, base_url: address.value.trim(), api_key: key.value,
      rpm: Number(rpm.value), max_concurrency: Number(concurrency.value) };
    if (jsonMode) payload.json_mode = jsonMode.checked;
    if (reasoningEffort) payload.reasoning_effort = reasoningEffort.value;
    if (referenceApi) payload.reference_api = referenceApi.value;
    if (savedConnectionId) payload.id = savedConnectionId;
    connectionStatus.textContent = '正在自动保存连接…'; delete connectionStatus.dataset.error;
    const result = await apiPost('/api/ai/connections', payload, signal);
    if (!active()) return;
    savedConnectionId = result.connection.id; key.value = '';
    connections = connections.filter(item => item.id !== savedConnectionId).concat(result.connection); updateChoices();
    connectionStatus.textContent = '连接已自动保存，可在上方加入模型池'; delete connectionStatus.dataset.error;
  };
  const scheduleConnection = debounce(() => saveConnection().catch(err => { connectionStatus.textContent = err.message || '连接自动保存失败'; connectionStatus.dataset.error = 'true'; }), 650);
  [connectionLabel, address, key, modelNames, rpm, concurrency, jsonMode, referenceApi, reasoningEffort].filter(Boolean).forEach(control => {
    control.addEventListener(control.type === 'checkbox' || control.tagName === 'SELECT' ? 'change' : 'input', scheduleConnection);
  });
  // Kept as an immediate fallback; normal editing saves automatically.
  button(newConnection, '立即保存连接', () => { scheduleConnection.cancel(); return saveConnection(); }, 'btn-primary');

  root.appendChild(el('h5', 'routing-section-title', '使用方式'));
  const strategies = el('div', 'routing-strategies'); strategies.setAttribute('role', 'group'); strategies.setAttribute('aria-label', name + '模型池：使用方式'); root.appendChild(strategies);
  const strategyNote = el('p', 'manga-note'); root.appendChild(strategyNote);
  const strategyButtons = [];
  const describeStrategy = () => {
    strategyButtons.forEach(([id, node]) => node.setAttribute('aria-pressed', String(strategy === id)));
    strategyNote.textContent = STRATEGIES.find(([id]) => id === strategy)?.[2] || '';
  };
  for (const [id, label] of STRATEGIES) {
    const node = button(strategies, label, () => { strategy = id; describeStrategy(); dirty(); });
    node.setAttribute('aria-label', name + '模型池：' + label); strategyButtons.push([id, node]);
  }
  describeStrategy();
  root.appendChild(el('h5', 'routing-section-title', '参数'));
  const advanced = el('details', 'routing-advanced'); advanced.open = false;
  advanced.appendChild(el('summary', null, '高级参数（超时、冷却、温度与 Token）'));
  const parameters = el('div', 'routing-parameters'); advanced.appendChild(parameters); root.appendChild(advanced);
  let temperature, maxTokens;
  if (kind === 'text') {
    temperature = field(parameters, '温度', config.temperature, 'number'); temperature.min = '0'; temperature.max = '2'; temperature.step = '0.1';
    maxTokens = field(parameters, '最大 Token', config.max_tokens, 'number'); maxTokens.min = '256'; maxTokens.max = '32768'; maxTokens.step = '1';
    const tokenHint = el('p', 'manga-note'); parameters.appendChild(tokenHint);
    updateTokenHint = () => {
      const reasoning = models.map(member => member.model).filter(model => REASONING.test(model));
      const value = Number(maxTokens.value) || config.max_tokens;
      tokenHint.textContent = reasoning.length
        ? '检测到推理模型 ' + reasoning.slice(0, 2).join('、') + '：思考过程会先占用最大 Token，建议 16000 以上（硬上限 32768）。'
        : '最大 Token 硬上限 32768。温度、最大 Token、超时与冷却对模型池内所有文本任务统一生效。';
      tokenHint.dataset.error = String(reasoning.length > 0 && value < 16000);
    };
    updateTokenHint();
  }
  const timeout = field(parameters, '单次请求超时（秒）', config.timeout, 'number'); timeout.min = '5'; timeout.max = '600'; timeout.step = '1';
  const cooldown = field(parameters, '失败模型冷却（秒）', config.cooldown, 'number'); cooldown.min = '0'; cooldown.max = '3600'; cooldown.step = '1';
  const presetWrap = el('label', 'manga-field'); presetWrap.appendChild(el('span', null, '策略预设'));
  const preset = el('select'); preset.setAttribute('aria-label', name + '模型池：策略预设');
  for (const item of presets) { const option = el('option', null, item.label + ' · ' + item.description); option.value = item.id; preset.appendChild(option); }
  preset.value = config.preset || 'balanced'; presetWrap.appendChild(preset); root.insertBefore(presetWrap, advanced);
  preset.addEventListener('change', () => {
    const selected = presets.find(item => item.id === preset.value); if (!selected) return;
    timeout.value = selected.timeout; cooldown.value = selected.cooldown;
    if (temperature) temperature.value = selected.temperature;
    if (maxTokens) maxTokens.value = selected.max_tokens;
    dirty('已选择“' + selected.label + '”预设，正在自动保存…');
  });
  root.appendChild(el('p', 'manga-note', '冷却中的模型会暂时跳过，冷却结束后自动参与后续请求。每个连接仍遵守各自的并发与 RPM 上限。单个模型在一次请求中最多调用一次，失败后切换候选；外层最多 3 轮。'));
  for (const control of [enabled, temperature, maxTokens, timeout, cooldown].filter(Boolean)) {
    control.addEventListener('input', () => { if (control === maxTokens) updateTokenHint(); dirty(); });
    control.addEventListener('change', () => { if (control === maxTokens) updateTokenHint(); dirty(); });
  }
  const actions = el('div', 'manga-actions'); root.appendChild(actions);
  button(actions, '立即保存', () => { clearTimeout(saveTimer); return saveNow(); }, 'btn-primary');
  button(actions, '重新加载模型池', () => refreshPool(true));
  if (kind === 'text') {
    const probe = el('section', 'automatic-diagnosis'); probe.hidden = true;
    const probeBody = el('div'); probe.appendChild(probeBody); root.appendChild(probe);
    button(actions, '检测文本 AI', async () => {
      probe.hidden = false;
      probeBody.replaceChildren(el('p', 'manga-note', '正在分级检测模型列表、对话、JSON 与原文引文…'));
      const result = await apiPost('/api/ai/probe', {}, signal);
      if (!active()) return;
      probeBody.replaceChildren(el('h5', null, '文本 AI 分级检测 · ' + result.model));
      for (const item of result.stages || []) {
        const row = el('article', 'automatic-diagnosis-item');
        row.dataset.level = item.ok ? 'ok' : item.fatal ? 'error' : 'warning';
        row.append(el('strong', null, (item.ok ? '通过 · ' : '未通过 · ') + item.title),
          el('p', 'manga-note', item.ok ? item.detail : (item.detail + '；' + (item.suggestion || '') + (item.ms ? '（' + item.ms + ' ms）' : ''))));
        probeBody.appendChild(row);
      }
      probeBody.appendChild(el('p', 'manga-note', '总耗时 ' + result.elapsed_ms + ' ms。' + (result.ok ? '可继续整书分析。' : '请先处理未通过项；非致命项只影响引文精度。')));
    });
  }
  root.appendChild(feedback);

  const onConnectionsChanged = () => { refreshPool(); };
  window.addEventListener('velmira-connections-changed', onConnectionsChanged);
  signal.addEventListener('abort', () => {
    window.removeEventListener('velmira-connections-changed', onConnectionsChanged);
    ++refreshVersion;
  }, { once: true });

  const statusPanel = el('section', 'routing-status'); root.appendChild(statusPanel);
  const statusTitle = el('div', 'routing-status-heading');
  statusTitle.appendChild(el('h5', null, '状态')); statusTitle.appendChild(el('small', null, '最近 24 小时')); statusPanel.appendChild(statusTitle);
  const statusContent = el('div'); statusPanel.appendChild(statusContent);
  const renderStatus = () => {
    statusContent.replaceChildren();
    if (status?.available === false) {
      statusContent.appendChild(el('p', 'manga-note', '暂时无法读取调用统计，模型池仍可使用'));
      return;
    }
    const summary = el('div', 'routing-status-summary');
    for (const [label, value] of [['请求', status?.requests ?? 0], ['模型调用', status?.attempts ?? 0], ['失败调用', status?.failures ?? 0]]) {
      const item = el('span'); item.appendChild(el('small', null, label)); item.appendChild(el('strong', null, String(value))); summary.appendChild(item);
    }
    statusContent.appendChild(summary);
    if (status?.last_model) statusContent.appendChild(el('p', 'manga-note', '最近使用：' + (typeof status.last_model === 'string' ? status.last_model : status.last_model.model || '')));
    if (!status?.models?.length) statusContent.appendChild(el('p', 'manga-note', '暂无调用记录。保存的模型池会在下次请求时参与调度。'));
    for (const model of status?.models || []) {
      const row = el('div', 'routing-status-model');
      const info = el('div'); info.appendChild(el('strong', null, model.model)); info.appendChild(el('small', null, model.label || connectionFor(model)?.label || model.connection_id)); row.appendChild(info);
      const rate = model.attempts ? (100 * model.failures / model.attempts).toFixed(1) : '0.0';
      row.appendChild(el('span', null, (Number(model.avg_ms || 0) / 1000).toFixed(2) + ' 秒 · 失败 ' + rate + '%'));
      if (model.cooldown_remaining > 0) row.appendChild(el('small', 'routing-cooling', '冷却 ' + Math.ceil(model.cooldown_remaining) + ' 秒'));
      if (model.last_error) row.appendChild(el('small', 'routing-last-error', model.last_error));
      statusContent.appendChild(row);
    }
  };
  renderStatus();
  button(statusPanel, '刷新' + name + '路由状态', async () => {
    const result = await apiGet('/api/ai/routing/' + kind, signal);
    if (active()) { status = result.status; renderStatus(); }
  });
}
