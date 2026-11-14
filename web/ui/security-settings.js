import { el, apiGet, apiPost, apiPatch, toast } from '../util/core.js';

export async function mountSecuritySettings({ area, signal }) {
  const panel = el('section', 'manga-studio'); area.replaceChildren(panel);
  panel.appendChild(el('h2', 'ai-section-title', '局域网访问'));
  let settings = (await apiGet('/api/security/settings', signal)).settings;
  if (signal?.aborted) return;
  const state = el('p', 'manga-note'); state.setAttribute('role', 'status'); panel.appendChild(state);
  const refresh = () => { state.textContent = settings.local ? (settings.enabled ? '局域网访问已开启；其他设备需要配对码。' : '局域网访问已关闭，仅本机可访问书库。') : '本设备已配对，可阅读书籍、保存进度和批注。模型和书库管理由电脑端操作。'; };
  refresh();
  if (!settings.local) {
    const logout = el('button', 'btn btn-danger', '退出本设备配对'); logout.type = 'button'; panel.appendChild(logout);
    logout.addEventListener('click', async () => {
      try { await apiPost('/api/auth/logout', {}); location.reload(); } catch (err) { toast(err.message); }
    });
    return;
  }
  panel.appendChild(el('p', 'manga-note', '推荐点击“连接新设备”后让手机扫描二维码；也可复制限时连接链接。手机与电脑需在同一网络，服务需使用 --host 0.0.0.0 启动。'));
  panel.appendChild(el('p', 'manga-note', '配对设备只能阅读和保存进度、书签、批注；API 密钥、模型设置、导入和生成管理仅限本机。更换配对码或关闭访问会撤销已配对设备。'));
  const actions = el('div', 'manga-tabs'); panel.appendChild(actions);
  const generate = el('button', 'btn btn-primary', '连接新设备'); generate.type = 'button'; actions.appendChild(generate);
  const toggle = el('button', settings.enabled ? 'btn btn-danger' : 'btn btn-primary', settings.enabled ? '关闭局域网访问' : '开启局域网访问'); toggle.type = 'button';
  toggle.disabled = !settings.token_configured; actions.appendChild(toggle);
  const rotate = el('button', 'btn btn-danger', '更换配对码并撤销已配对设备'); rotate.type = 'button'; rotate.hidden = !settings.token_configured; actions.appendChild(rotate);
  const tokenArea = el('div'); tokenArea.hidden = true; panel.appendChild(tokenArea);
  const tokenNote = el('p', 'manga-note', '配对码仅在新建或更换时显示。二维码邀请不会撤销已配对设备。'); tokenArea.appendChild(tokenNote);
  const label = el('label', 'manga-field'); label.appendChild(el('span', null, '新配对码'));
  const code = el('input'); code.type = 'text'; code.readOnly = true; code.autocomplete = 'off'; code.spellcheck = false;
  code.setAttribute('aria-label', '新配对码'); label.appendChild(code); tokenArea.appendChild(label);
  const copy = el('button', 'btn', '复制配对码'); copy.type = 'button'; tokenArea.appendChild(copy);
  copy.addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(code.value); toast('配对码已复制'); }
    catch { code.focus(); code.select(); toast('请复制已选中的配对码'); }
  });
  const pairing = el('div', 'security-pairing'); pairing.hidden = true; tokenArea.appendChild(pairing);
  pairing.appendChild(el('strong', null, '扫码连接'));
  pairing.appendChild(el('p', 'manga-note', '手机与电脑连接同一 Wi-Fi 后，用相机扫描二维码即可配对。链接 5 分钟内有效且只能使用一次，不包含模型密钥。'));
  const qr = el('img', 'security-qr'); qr.alt = 'Velmira 局域网配对二维码'; qr.hidden = true; pairing.appendChild(qr);
  const linkLabel = el('label', 'manga-field'); linkLabel.appendChild(el('span', null, '连接链接（二维码不可用时复制）'));
  const link = el('input'); link.type = 'text'; link.readOnly = true; link.autocomplete = 'off'; link.spellcheck = false; link.setAttribute('aria-label', '连接链接'); linkLabel.appendChild(link); pairing.appendChild(linkLabel);
  const copyLink = el('button', 'btn', '复制连接链接'); copyLink.type = 'button'; pairing.appendChild(copyLink);
  copyLink.addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(link.value); toast('连接链接已复制'); }
    catch { link.focus(); link.select(); toast('请复制已选中的连接链接'); }
  });
  let qrUrl = '', expiryTimer;
  const renderPairing = async (pairingUrl) => {
    link.value = pairingUrl || '';
    pairing.hidden = !pairingUrl;
    qr.hidden = true;
    clearTimeout(expiryTimer);
    if (qrUrl) { URL.revokeObjectURL(qrUrl); qrUrl = ''; }
    if (!pairingUrl) return;
    expiryTimer = setTimeout(() => { link.value = ''; qr.hidden = true; if (qrUrl) { URL.revokeObjectURL(qrUrl); qrUrl = ''; } tokenNote.textContent = '连接邀请已过期，请重新点击“连接新设备”。'; }, 5 * 60 * 1000);
    try {
      const response = await fetch('/api/security/qr', { method: 'POST', credentials: 'same-origin', cache: 'no-store',
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ data: pairingUrl }), signal });
      if (!response.ok) return;
      const blob = await response.blob(); qrUrl = URL.createObjectURL(blob); qr.src = qrUrl; qr.hidden = false;
    } catch (_) { /* optional QR component; the copyable link remains available */ }
  };
  let busy = false;
  const update = () => { refresh(); toggle.textContent = settings.enabled ? '关闭局域网访问' : '开启局域网访问'; toggle.className = settings.enabled ? 'btn btn-danger' : 'btn btn-primary'; toggle.disabled = !settings.token_configured; rotate.hidden = !settings.token_configured; };
  async function createInvitation(endpoint, revoke = false) {
    if (busy) return; busy = true; generate.disabled = true;
    try {
      const result = await apiPost(endpoint, {}, signal);
      if (signal?.aborted) return;
      settings = result.settings; code.value = result.token || ''; label.hidden = copy.hidden = !result.token;
      tokenNote.textContent = result.token ? '备用配对码仅本次显示。扫码邀请 5 分钟内有效，使用一次即失效。' : '已有设备保持连接。扫码邀请 5 分钟内有效，使用一次即失效。';
      tokenArea.hidden = false; update(); await renderPairing(result.pairing_url); toast(revoke ? '旧设备已撤销，新配对码已生成' : '连接邀请已生成');
    } catch (err) { if (!signal?.aborted) toast(err.message); }
    finally { busy = false; generate.disabled = false; }
  }
  generate.addEventListener('click', () => createInvitation('/api/security/invite'));
  rotate.addEventListener('click', () => createInvitation('/api/security/token', true));
  toggle.addEventListener('click', async () => {
    if (busy) return; busy = true; toggle.disabled = true;
    try {
      const result = await apiPatch('/api/security/settings', { enabled: !settings.enabled }, signal);
      if (signal?.aborted) return;
      settings = result.settings; code.value = ''; tokenArea.hidden = true; await renderPairing(''); update();
    } catch (err) { if (!signal?.aborted) toast(err.message); }
    finally { busy = false; toggle.disabled = !settings.token_configured; }
  });
  signal?.addEventListener('abort', () => { code.value = ''; link.value = ''; clearTimeout(expiryTimer); if (qrUrl) URL.revokeObjectURL(qrUrl); }, { once: true });
}
