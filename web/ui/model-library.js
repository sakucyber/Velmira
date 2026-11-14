import { el, apiGet, apiPost, apiPatch, apiDelete, debounce } from '../util/core.js';

const CAPABILITIES = { text: '文本', image: '生图', vision: '视觉', embedding: '嵌入', tools: '工具' };
let stylesheet;
function loadStyles() {
  if (!stylesheet) stylesheet = new Promise((resolve, reject) => {
    const existing = document.getElementById('model-library-styles');
    if (existing?.sheet) { resolve(); return; }
    const link = existing || el('link'); link.id = 'model-library-styles'; link.rel = 'stylesheet'; link.href = new URL('../styles/model-library.css', import.meta.url).href;
    link.addEventListener('load', resolve, { once: true }); link.addEventListener('error', () => { stylesheet = null; reject(new Error('模型库样式加载失败')); }, { once: true });
    if (!existing) document.head.appendChild(link);
  });
  return stylesheet;
}

/** Independent of books: connection credentials never enter browser responses. */
export async function mountModelLibrary({ area, signal }) {
  const root = el('section', 'model-library'); area.replaceChildren(root);
  const feedback = el('p', 'ml-feedback'); feedback.setAttribute('role', 'status'); feedback.setAttribute('aria-live', 'polite');
  let data = { vendors: [], models: [] }, selected = '', query = '', filter = 'all', busy = false;
  const active = () => !signal.aborted && root.isConnected;
  const endpoint = '/api/ai/catalog';
  const tell = (message, error = false) => { feedback.textContent = message; feedback.dataset.error = String(error); };
  async function run(callback) {
    if (busy || !active()) return;
    busy = true; root.inert = true; root.setAttribute('aria-busy', 'true');
    try { await callback(); } catch (err) { if (active()) tell(err.message || '操作失败', true); }
    finally { busy = false; root.inert = false; root.removeAttribute('aria-busy'); }
  }
  function button(parent, text, callback, cls = '') {
    const node = el('button', 'btn ' + cls, text); node.type = 'button'; parent.appendChild(node);
    node.addEventListener('click', () => run(callback)); return node;
  }
  function input(parent, label, value = '', type = 'text') {
    const wrap = el('label', 'ml-field'); wrap.appendChild(el('span', null, label));
    const node = el('input'); node.type = type; node.value = value ?? ''; node.setAttribute('aria-label', '模型库：' + label);
    wrap.appendChild(node); parent.appendChild(wrap); return node;
  }
  function select(parent, label, choices, value) {
    const wrap = el('label', 'ml-field'); wrap.appendChild(el('span', null, label));
    const node = el('select'); node.setAttribute('aria-label', '模型库：' + label);
    choices.forEach(([id, text]) => { const option = el('option', null, text); option.value = id; node.appendChild(option); });
    if (value != null) node.value = value;
    wrap.appendChild(node); parent.appendChild(wrap); return node;
  }
  async function refresh(message) {
    data = await apiGet(endpoint, signal);
    if (!active()) return;
    if (selected && !data.vendors.some(v => v.id === selected)) selected = '';
    render(); if (message) tell(message);
    window.dispatchEvent(new CustomEvent('velmira-connections-changed'));
  }
  let body, editor, tableArea;
  function editorPanel(title) {
    editor.replaceChildren(); editor.hidden = false;
    const header = el('div', 'ml-editor-heading'); header.appendChild(el('h3', null, title));
    button(header, '取消', () => { editor.hidden = true; editor.replaceChildren(); }); editor.appendChild(header);
    editor.scrollIntoView({ block: 'nearest' }); return editor;
  }
  function confirmDelete(title, description, action) {
    const panel = editorPanel(title); panel.appendChild(el('p', 'ml-help', description));
    button(panel, '确认删除', action, 'ml-danger');
  }
  function vendorForm(old = null) {
    const panel = editorPanel(old ? '编辑厂商连接' : '添加厂商连接');
    panel.appendChild(el('p', 'ml-help', '同一站点的文本与生图服务可分别保存连接。填写后会自动保存，密钥直接保存在本机配置中。'));
    const grid = el('div', 'ml-form-grid'); panel.appendChild(grid);
    const label = input(grid, '厂商名称', old?.label);
    const kind = select(grid, '连接类型', [['text', '文本 API'], ['image', '生图 API']], old?.kind || 'text'); kind.disabled = !!old;
    const address = input(grid, 'API 地址（含 /v1 等前缀）', old?.base_url); address.placeholder = 'http://localhost:11434/v1 或 https://example.com/v1';
    const key = input(grid, 'API 密钥（留空保留）', '', 'password'); key.autocomplete = 'new-password';
    const rpm = input(grid, '每分钟请求上限（0 为不限）', old?.rpm ?? 0, 'number'); rpm.min = '0'; rpm.max = '1000';
    const concurrency = input(grid, '最大并发', old?.max_concurrency ?? 1, 'number'); concurrency.min = '1'; concurrency.max = '8';
    const json = input(grid, '要求 JSON 输出（服务需支持）', '', 'checkbox'); json.checked = old?.json_mode ?? true;
    const reference = select(grid, '参考图接口', [['edits', '标准图像编辑接口'], ['generations', 'Agnes JSON 参考图接口']], old?.reference_api || 'edits');
    const adapt = () => { json.parentElement.hidden = kind.value !== 'text'; reference.parentElement.hidden = kind.value !== 'image'; };
    kind.addEventListener('change', adapt); adapt();
    if (old?.key_configured) panel.appendChild(el('p', 'ml-help', '此连接已保存密钥。修改 API 地址时请重新填写密钥。'));
    const autoStatus = el('p', 'ml-help ml-autosave'); autoStatus.setAttribute('role', 'status'); panel.appendChild(autoStatus);
    let connectionId = old?.id || '';
    const save = async () => {
      if (!label.value.trim() || !address.value.trim()) throw new Error('请填写厂商名称和 API 地址');
      const payload = { label: label.value.trim(), kind: kind.value, base_url: address.value.trim(), api_key: key.value,
        models: old?.models || [], rpm: Number(rpm.value), max_concurrency: Number(concurrency.value), json_mode: json.checked, reference_api: reference.value };
      if (connectionId) payload.id = connectionId;
      autoStatus.textContent = '正在自动保存…'; delete autoStatus.dataset.error;
      const result = await apiPost('/api/ai/connections', payload, signal);
      connectionId = result.connection.id; selected = connectionId; key.value = '';
      autoStatus.textContent = '已自动保存';
      data.vendors = data.vendors.filter(v => v.id !== connectionId).concat(result.connection);
      if (tableArea) renderTable();
      window.dispatchEvent(new CustomEvent('velmira-connections-changed'));
    };
    const schedule = debounce(() => save().catch(err => { autoStatus.textContent = err.message || '自动保存失败'; autoStatus.dataset.error = 'true'; }), 650);
    [label, kind, address, key, rpm, concurrency, json, reference].forEach(node => node.addEventListener(node.type === 'checkbox' || node.tagName === 'SELECT' ? 'change' : 'input', schedule));
    // Keep a manual action as an immediate fallback, but no click is required.
    button(panel, '保存厂商连接', async () => { schedule.cancel(); await save(); await refresh('厂商连接已保存，可添加模型或读取供应商列表'); }, 'btn-primary');
    label.focus();
  }
  function modelForm(old = null) {
    if (!data.vendors.length) { vendorForm(); tell('请先保存一个厂商连接'); return; }
    const panel = editorPanel(old ? '编辑模型' : '添加模型');
    const grid = el('div', 'ml-form-grid'); panel.appendChild(grid);
    const vendor = select(grid, '模型厂商', data.vendors.map(v => [v.id, v.label]), old?.connection_id || selected || data.vendors[0].id);
    const model = input(grid, '模型 ID', old?.model); model.placeholder = '供应商接口中的模型名称';
    const label = input(grid, '模型显示名', old?.label);
    const inputPrice = input(grid, '输入价格（可留空）', old?.input_price, 'number'); inputPrice.min = '0'; inputPrice.step = 'any';
    const outputPrice = input(grid, '输出价格（可留空）', old?.output_price, 'number'); outputPrice.min = '0'; outputPrice.step = 'any';
    panel.appendChild(el('p', 'ml-help', '输入模型 ID 后会自动填写“厂商/模型 ID”作为显示名。填写后会自动保存；价格为手填参考值，未知时留空。'));
    const capabilities = el('fieldset', 'ml-capability-fields'); capabilities.appendChild(el('legend', null, '能力'));
    const caps = {};
    for (const [id, name] of Object.entries(CAPABILITIES)) { caps[id] = input(capabilities, name, '', 'checkbox'); caps[id].checked = old ? old.capabilities.includes(id) : data.vendors.find(v => v.id === vendor.value)?.kind === id; }
    panel.appendChild(capabilities);
    const autoStatus = el('p', 'ml-help ml-autosave'); autoStatus.setAttribute('role', 'status'); panel.appendChild(autoStatus);
    const initialVendor = data.vendors.find(v => v.id === (old?.connection_id || vendor.value));
    let autoLabel = old?.label || '';
    let labelEdited = !!old && old.label !== ((initialVendor?.label || old.connection_id) + '/' + old.model);
    const updateLabel = () => {
      const vendorItem = data.vendors.find(v => v.id === vendor.value);
      const next = model.value.trim() ? (vendorItem?.label || vendor.value) + '/' + model.value.trim() : '';
      if (!labelEdited || !label.value.trim() || label.value === autoLabel) { label.value = next; autoLabel = next; }
    };
    label.addEventListener('input', () => { labelEdited = true; });
    model.addEventListener('input', updateLabel); vendor.addEventListener('change', updateLabel); updateLabel();
    let saveTimer = 0;
    const save = async () => {
      if (!model.value.trim()) return;
      const payload = { connection_id: vendor.value, model: model.value.trim(), label: label.value.trim() || ((data.vendors.find(v => v.id === vendor.value)?.label || vendor.value) + '/' + model.value.trim()),
        capabilities: Object.keys(caps).filter(id => caps[id].checked),
        input_price: inputPrice.value === '' ? null : Number(inputPrice.value), output_price: outputPrice.value === '' ? null : Number(outputPrice.value) };
      if (!payload.capabilities.length) throw new Error('请至少选择一项模型能力');
      autoStatus.textContent = '正在自动保存…'; delete autoStatus.dataset.error;
      const previousId = old?.id;
      const result = old ? await apiPatch(endpoint + '/models/' + encodeURIComponent(previousId), payload, signal) : await apiPost(endpoint + '/models', payload, signal);
      autoStatus.textContent = '已自动保存';
      if (result.model) {
        old = result.model;
        data.models = data.models.filter(item => item.id !== previousId && item.id !== result.model.id).concat(result.model);
        if (tableArea) renderTable();
      }
      window.dispatchEvent(new CustomEvent('velmira-connections-changed'));
    };
    const schedule = () => { clearTimeout(saveTimer); saveTimer = setTimeout(() => save().catch(err => { autoStatus.textContent = err.message || '自动保存失败'; autoStatus.dataset.error = 'true'; }), 650); };
    [vendor, model, label, inputPrice, outputPrice, ...Object.values(caps)].forEach(node => node.addEventListener(node.type === 'checkbox' || node.tagName === 'SELECT' ? 'change' : 'input', schedule));
    button(panel, '保存模型', async () => { clearTimeout(saveTimer); await save(); await refresh('模型已保存'); }, 'btn-primary');
    model.focus();
  }
  async function importModels(item) {
    tell('正在读取供应商模型列表…');
    const result = await apiGet(endpoint + '/vendors/' + encodeURIComponent(item.id) + '/models', signal);
    if (!active()) return;
    const panel = editorPanel('从 ' + item.label + ' 导入模型');
    panel.appendChild(el('p', 'ml-help', '只读取 /models，不执行生成。请选择需要的模型；能力标签可在导入后编辑。'));
    const list = el('div', 'ml-import-list'); panel.appendChild(list);
    const candidates = result.models.filter(m => !item.models.includes(m));
    const checks = candidates.map(model => { const check = input(list, model, '', 'checkbox'); check.dataset.importModel = model; return check; });
    if (!candidates.length) panel.appendChild(el('p', 'ml-help', result.models.length ? '列表中的模型已全部添加。' : '供应商返回了空模型列表，可手动添加。'));
    const actions = el('div', 'ml-actions'); panel.appendChild(actions);
    button(actions, '全选', () => { checks.forEach(c => { c.checked = true; }); });
    button(actions, '取消全选', () => { checks.forEach(c => { c.checked = false; }); });
    button(actions, '导入所选模型', async () => {
      const names = checks.filter(c => c.checked).map(c => c.dataset.importModel);
      if (!names.length) throw new Error('请至少选择一个模型');
      // One atomic connection update imports names; metadata is written as well
      // so imported rows immediately use the vendor/model display name.
      await apiPost('/api/ai/connections', { id: item.id, label: item.label, kind: item.kind, models: [...item.models, ...names] }, signal);
      await Promise.all(names.map(name => apiPost(endpoint + '/models', {
        connection_id: item.id, model: name, label: item.label + '/' + name, capabilities: [item.kind], input_price: null, output_price: null,
      }, signal).catch(err => { if (err.code !== 'catalog_model_conflict') throw err; })));
      await refresh('已导入 ' + names.length + ' 个模型');
    }, 'btn-primary');
    tell('已读取 ' + result.models.length + ' 个模型');
  }
  async function addToPool(model, kind) {
    const name = kind === 'text' ? '文本' : '生图';
    const result = await apiPost(endpoint + '/models/' + encodeURIComponent(model.id) + '/pool', { kind }, signal);
    tell('已将 ' + model.label + ' 添加到' + name + '模型池');
    // Keep the catalog assignments in sync without disturbing an open editor.
    data = await apiGet(endpoint, signal); if (active()) renderTable();
    window.dispatchEvent(new CustomEvent('velmira-connections-changed'));
    return result;
  }
  function renderTable() {
    tableArea.replaceChildren();
    const vendors = new Map(data.vendors.map(v => [v.id, v]));
    const matched = data.models.filter(m => (!selected || m.connection_id === selected)
      && (filter === 'all' || filter === 'used' ? filter !== 'used' || m.assignments.length : m.capabilities.includes(filter))
      && (!query || [m.label, m.model, vendors.get(m.connection_id)?.label || ''].some(value => value.toLowerCase().includes(query.toLowerCase()))));
    tableArea.appendChild(el('p', 'ml-results', matched.length + ' 个模型'));
    if (!matched.length) { tableArea.appendChild(el('p', 'ml-empty', data.models.length ? '没有符合条件的模型。' : '还没有模型，先添加厂商连接，再添加或导入模型。')); return; }
    const table = el('div', 'ml-table'); table.setAttribute('role', 'table'); table.setAttribute('aria-label', '模型库'); tableArea.appendChild(table);
    const head = el('div', 'ml-table-row ml-table-head'); head.setAttribute('role', 'row');
    for (const text of ['模型 / ID', '厂商', '能力', '价格（输入 / 输出）', '已分配工作', '操作']) { const cell = el('div', null, text); cell.setAttribute('role', 'columnheader'); head.appendChild(cell); } table.appendChild(head);
    matched.forEach(model => {
      const row = el('div', 'ml-table-row'); row.setAttribute('role', 'row'); row.dataset.modelId = model.id;
      const title = el('div', 'ml-model-name'); title.appendChild(el('strong', null, model.label)); title.appendChild(el('code', null, model.model));
      const vendor = el('div', 'ml-model-vendor', vendors.get(model.connection_id)?.label || model.connection_id);
      const caps = el('div', 'ml-badges'); model.capabilities.forEach(c => caps.appendChild(el('span', 'ml-badge', CAPABILITIES[c] || c)));
      const price = el('div', 'ml-price', (model.input_price ?? '—') + ' / ' + (model.output_price ?? '—'));
      const assignments = el('div', 'ml-assignments'); model.assignments.forEach(a => assignments.appendChild(el('span', 'ml-assignment', a))); if (!model.assignments.length) assignments.textContent = '未分配';
      const actions = el('div', 'ml-row-actions');
      const vendorItem = vendors.get(model.connection_id);
      const poolKind = vendorItem?.kind === 'text' || vendorItem?.kind === 'image' ? vendorItem.kind : null;
      if (poolKind) {
        const poolName = poolKind === 'text' ? '文本' : '生图';
        const assigned = model.assignments.some(a => a.startsWith(poolName + '模型池'));
        const add = button(actions, assigned ? '已在' + poolName + '池' : '加入' + poolName + '池', () => addToPool(model, poolKind), assigned ? 'ml-pool-added' : 'ml-pool-add');
        add.disabled = assigned;
        add.title = assigned ? '此模型已经在' + poolName + '模型池中' : '直接添加到' + poolName + '模型池';
      }
      button(actions, '检测', async () => { const result = await apiPost(endpoint + '/models/' + encodeURIComponent(model.id) + '/probe', {}, signal); tell(result.note, !result.ok); });
      button(actions, '编辑', () => modelForm(model));
      button(actions, '删除', () => confirmDelete('删除模型', '删除 ' + model.label + '？已加入模型池的模型需要先从池中移除。', async () => { await apiDelete(endpoint + '/models/' + encodeURIComponent(model.id), signal); await refresh('模型已删除'); }), 'ml-danger');
      [title, vendor, caps, price, assignments, actions].forEach((cell, i) => { cell.setAttribute('role', 'cell'); cell.dataset.label = ['模型', '厂商', '能力', '价格', '工作', '操作'][i]; row.appendChild(cell); }); table.appendChild(row);
    });
  }
  function render() {
    root.replaceChildren();
    const heading = el('div', 'ml-heading'); const text = el('div'); text.appendChild(el('h2', null, '模型库'));
    text.appendChild(el('p', 'ml-help', data.models.length + ' 个模型 · ' + data.vendors.length + ' 个厂商连接')); heading.appendChild(text);
    const actions = el('div', 'ml-actions'); heading.appendChild(actions);
    button(actions, '刷新模型库', () => refresh('模型库已刷新')); button(actions, '添加厂商', () => vendorForm()); button(actions, '添加模型', () => modelForm(), 'btn-primary'); root.appendChild(heading);
    root.appendChild(feedback);
    editor = el('section', 'ml-editor'); editor.hidden = true; root.appendChild(editor);
    body = el('div', 'ml-layout'); root.appendChild(body);
    const sidebar = el('aside', 'ml-vendors'); sidebar.setAttribute('aria-label', '模型厂商'); body.appendChild(sidebar);
    function vendorChoice(id, label, count, address = '') {
      const node = button(sidebar, '', () => { selected = id; render(); }); node.className = 'ml-vendor'; node.dataset.vendor = id;
      node.setAttribute('aria-pressed', String(selected === id)); const top = el('span', 'ml-vendor-name'); top.appendChild(el('strong', null, label)); top.appendChild(el('small', null, count)); node.appendChild(top);
      if (address) node.appendChild(el('small', 'ml-vendor-address', address));
    }
    vendorChoice('', '全部厂商', data.models.length);
    data.vendors.forEach(v => vendorChoice(v.id, v.label, v.models.length, v.base_url));
    const main = el('div', 'ml-main'); body.appendChild(main);
    const selectedVendor = data.vendors.find(v => v.id === selected);
    if (selectedVendor) {
      const tools = el('div', 'ml-vendor-tools'); tools.appendChild(el('strong', null, selectedVendor.label));
      button(tools, '读取供应商模型', () => importModels(selectedVendor)); button(tools, '编辑厂商', () => vendorForm(selectedVendor));
      button(tools, '删除厂商', () => confirmDelete('删除厂商连接', '将删除此厂商的全部模型和本机保存的密钥。已加入模型池的模型需先移除。', async () => { await apiDelete(endpoint + '/vendors/' + encodeURIComponent(selectedVendor.id), signal); selected = ''; await refresh('厂商连接已删除'); }), 'ml-danger'); main.appendChild(tools);
    }
    const search = input(main, '搜索模型名称、ID 或厂商', query); search.type = 'search'; search.placeholder = '搜索模型名称、ID 或厂商';
    search.addEventListener('input', () => { query = search.value; renderTable(); });
    const filters = el('div', 'ml-filters'); filters.setAttribute('role', 'group'); filters.setAttribute('aria-label', '模型能力筛选'); main.appendChild(filters);
    for (const [id, label] of [['all', '全部'], ['used', '使用中'], ...Object.entries(CAPABILITIES)]) {
      const node = button(filters, label, () => { filter = id; [...filters.children].forEach(b => b.setAttribute('aria-pressed', String(b === node))); renderTable(); }); node.dataset.filter = id; node.setAttribute('aria-pressed', String(filter === id));
    }
    tableArea = el('div', 'ml-table-area'); main.appendChild(tableArea); renderTable();
  }
  root.appendChild(el('p', 'ml-help', '正在读取模型库…'));
  try { await loadStyles(); await refresh(); }
  catch (err) { if (active()) { root.replaceChildren(feedback); tell('模型库加载失败：' + err.message, true); } }
}
