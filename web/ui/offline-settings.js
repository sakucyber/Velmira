import { el, apiGet, apiDelete, getAuthState, toast } from '../util/core.js';
import { localStore, deviceId } from '../util/local-store.js';
import { downloadImage, removeImages } from '../util/local-media.js';

export async function mountOfflineSettings({ area, signal }) {
  if (getAuthState().standalone) return mountStandaloneLibrarySettings({ area, signal });
  const root = el('section', 'manga-studio offline-settings'); area.replaceChildren(root);
  root.append(el('h3', null, '离线与同步'), el('p', 'settings-help', '把书下载到当前设备，断网后继续阅读、搜索、写批注和书签。联网后同步到当前服务；每台设备需单独下载。'));
  if (/VelmiraAndroid\//.test(navigator.userAgent)) {
    const home = el('a', 'btn', '返回手机本机书库'); home.href = 'velmira://local'; root.append(home);
  }
  const device = el('p', 'settings-help', '本设备：' + deviceId); device.style.overflowWrap = 'anywhere'; root.append(device);
  const status = el('p', 'manga-note offline-status'); status.setAttribute('role', 'status'); root.append(status);
  const actions = el('div', 'manga-actions'), sync = el('button', 'btn btn-primary', '立即同步'), refresh = el('button', 'btn', '刷新列表');
  const downloadAll = el('button', 'btn btn-primary', '全部下载到本设备'); downloadAll.type = 'button'; downloadAll.setAttribute('aria-label', '全部下载到本设备'); downloadAll.hidden = true;
  sync.type = refresh.type = 'button'; actions.append(sync, downloadAll, refresh); root.append(actions);
  const autoLabel = el('label', 'manga-field'), auto = el('input'); auto.type = 'checkbox'; auto.setAttribute('aria-label', '联网后自动同步');
  auto.checked = (await localStore.meta('auto-sync')) !== false;
  autoLabel.append(auto, el('span', null, '联网后自动同步阅读记录')); root.append(autoLabel);
  auto.addEventListener('change', () => localStore.meta('auto-sync', auto.checked));
  const storage = el('p', 'settings-help'); root.append(storage);
  const list = el('div', 'offline-book-list'); root.append(list);
  const conflicts = el('div', 'offline-conflicts'); root.append(conflicts);
  const devices = el('div', 'offline-devices'); root.append(devices);
  const reset = el('button', 'btn btn-danger', '重新注册本设备'); reset.type = 'button';
  root.append(el('p', 'settings-help', '设备被撤销后，先重新配对，再重新注册。本机尚未同步的阅读记录会保留。'), reset);
  reset.addEventListener('click', async () => {
    if (busy) return; busy = true; reset.disabled = true;
    try { await localStore.resetDevice(); location.reload(); }
    catch (err) { status.textContent = err.message; }
    finally { busy = false; reset.disabled = false; }
  });
  let busy = false;
  let currentBooks = [];
  const setStatus = (message, state = '') => { status.textContent = message; status.dataset.state = state; };
  async function summary() {
    const pending = await localStore.allPendingOps();
    setStatus((navigator.onLine ? '网络已连接' : '当前离线') + ' · 待同步 ' + pending.length + ' 条', navigator.onLine ? 'online' : 'offline');
    const last = await localStore.meta('sync:last');
    if (last?.at) status.textContent += ' · 上次同步 ' + new Date(last.at).toLocaleString();
    const estimate = await navigator.storage?.estimate?.().catch(() => null);
    storage.textContent = estimate ? `本设备存储：已用 ${(estimate.usage / 1048576).toFixed(1)} MB / 配额 ${(estimate.quota / 1048576).toFixed(0)} MB` : '离线数据保存在当前浏览器或应用中。';
    const saved = (await localStore.meta('sync:conflicts')) || [];
    conflicts.replaceChildren();
    if (saved.length) {
      conflicts.append(el('h4', null, '待处理冲突'), el('p', 'settings-help', '服务器与离线版本都有修改，已保留你的离线副本。采用服务器版本会放弃这条本机修改；保留本机版本会在下次同步时重新提交。'));
      for (const item of saved) {
        const card = el('article', 'ai-settings-card');
        const operation = pending.find(op => op.opId === item.op_id);
        card.append(el('strong', null, `${({ annotation: '批注', bookmark: '书签', progress: '进度' })[item.entity] || item.entity} · ${item.entity_id || ''}`), el('pre', 'offline-conflict-content', JSON.stringify(operation?.payload || item.payload || item, null, 2)));
        const choices = el('div', 'ai-settings-actions');
        if (item.reason === 'duplicate_bookmark') card.append(el('p', 'settings-help', '这个位置已有书签，采用已有书签即可完成处理。'));
        const options = item.reason === 'duplicate_bookmark' ? [['server', '采用已有书签']] : [['local', '保留本机版本'], ['server', '采用服务器版本']];
        for (const [choice, label] of options) {
          const button = el('button', 'btn', label); button.type = 'button';
          button.addEventListener('click', async () => {
            if (busy) return; busy = true; button.disabled = true;
            try { await localStore.resolveConflict(item.op_id, choice); await summary(); status.textContent += ' · 选择已保存，请同步完成处理'; }
            catch (err) { status.textContent = err.message; }
            finally { busy = false; button.disabled = false; }
          });
          choices.append(button);
        }
        card.append(choices);
        conflicts.append(card);
      }
    }
  }
  async function load() {
    list.replaceChildren(el('p', 'settings-help', '正在读取书库…'));
    let books;
    try { books = (await apiGet('/api/books', signal)).books; }
    catch { books = await localStore.listBooks(); }
    if (signal.aborted) return;
    list.replaceChildren();
    currentBooks = books;
    downloadAll.hidden = !books.length;
    if (!books.length) list.append(el('p', 'settings-help', '连接服务并导入书籍后，可在这里下载。'));
    for (const book of books) {
      const row = el('article', 'ai-settings-card offline-book'), info = el('div');
      const mark = await localStore.meta('download:' + book.id);
      info.append(el('strong', null, book.title), el('p', 'settings-help', mark?.complete ? `已下载 · ${mark.count} 章/页` : '尚未完整下载'));
      const download = el('button', 'btn', mark?.complete ? '重新下载' : '下载到本设备'); download.type = 'button'; download.setAttribute('aria-label', '下载 ' + book.title);
      download.addEventListener('click', async () => {
        if (busy) return; busy = true; download.disabled = true;
        try {
          await navigator.storage?.persist?.();
          await downloadBook(book, message => { setStatus(message, 'busy'); }, signal);
          toast('已下载到本设备'); await load();
        } catch (err) { if (!signal.aborted) { setStatus(err.message, 'error'); toast(err.message); } }
        finally { busy = false; download.disabled = false; }
      });
      const controls = el('div', 'ai-settings-actions'); controls.append(download);
      const remove = el('button', 'btn btn-danger', '清除本机下载'); remove.type = 'button'; remove.setAttribute('aria-label', '清除下载 ' + book.title);
      remove.addEventListener('click', () => {
        if (busy || row.querySelector('.offline-remove-confirm')) return;
        const confirmation = el('div', 'offline-remove-confirm');
        confirmation.append(el('p', 'settings-help', '清除本设备的正文、图片和阅读缓存。服务端书籍会保留；有未同步修改时无法清除。'));
        const yes = el('button', 'btn btn-danger', '确认清除'), no = el('button', 'btn', '取消'); yes.type = no.type = 'button';
        confirmation.append(yes, no); row.append(confirmation); no.addEventListener('click', () => confirmation.remove());
        yes.addEventListener('click', async () => {
          if (busy) return; busy = true; yes.disabled = true;
          try { await localStore.removeBook(book.id); await removeImages(book.id); toast('本机下载已清除'); await load(); }
          catch (err) { status.textContent = err.message; }
          finally { busy = false; yes.disabled = false; }
        });
      });
      controls.append(remove); row.append(info, controls); list.append(row);
    }
    await summary();
  }
  downloadAll.addEventListener('click', async () => {
    if (busy || !currentBooks.length) return;
    busy = true; downloadAll.disabled = true; sync.disabled = true; refresh.disabled = true;
    let completed = 0;
    try {
      await navigator.storage?.persist?.();
      const pendingBooks = [];
      for (const book of currentBooks) {
        const mark = await localStore.meta('download:' + book.id);
        if (!mark?.complete) pendingBooks.push(book);
      }
      if (!pendingBooks.length) { setStatus('所有书籍都已下载到本设备', 'online'); return; }
      for (const book of pendingBooks) {
        if (signal.aborted) return;
        await downloadBook(book, message => { setStatus('批量下载 · ' + message, 'busy'); }, signal);
        completed += 1;
      }
      toast('已完成 ' + completed + ' 本书的离线下载'); await load();
    } catch (err) {
      if (!signal.aborted) { setStatus('批量下载未完成：' + err.message + '；可以稍后继续', 'error'); toast(err.message); }
    } finally { busy = false; downloadAll.disabled = false; sync.disabled = false; refresh.disabled = false; }
  });
  sync.addEventListener('click', async () => {
    if (busy) return; busy = true; sync.disabled = true; downloadAll.disabled = true; setStatus('正在同步…', 'busy');
    try { const result = await localStore.sync({ signal }); await summary(); if (!signal.aborted) setStatus(status.textContent + ` · 本次已同步 ${(result.accepted || []).length} 条`, 'online'); }
    catch (err) { if (!signal.aborted) setStatus('同步未完成：' + err.message + '；离线操作会保留。', 'error'); }
    finally { busy = false; sync.disabled = false; downloadAll.disabled = false; }
  });
  refresh.addEventListener('click', () => { if (!busy) load().catch(err => { status.textContent = err.message; }); });
  if (getAuthState().local) {
    devices.append(el('h4', null, '已注册设备'));
    try {
      for (const item of (await apiGet('/api/sync/devices', signal)).devices || []) {
        const card = el('article', 'ai-settings-card'); card.append(el('strong', null, item.label || '未命名设备'), el('p', 'settings-help', item.id + (item.id === deviceId ? '（本设备）' : '')));
        if (item.id === 'server') card.append(el('p', 'settings-help', '服务端记录'));
        else if (item.revoked_at) card.append(el('p', 'settings-help', '已撤销'));
        else {
          const revoke = el('button', 'btn', '撤销设备'); revoke.type = 'button';
          revoke.addEventListener('click', async () => {
            if (busy) return; busy = true; revoke.disabled = true;
            try { await apiDelete('/api/sync/devices/' + encodeURIComponent(item.id), signal); revoke.textContent = '已撤销'; status.textContent = '已停止此设备的在线访问；已下载内容需在该设备清除。'; }
            catch (err) { status.textContent = err.message; revoke.disabled = false; }
            finally { busy = false; }
          }); card.append(revoke);
        }
        devices.append(card);
      }
    } catch (err) { if (!signal.aborted) devices.append(el('p', 'settings-help', '设备列表暂不可用：' + err.message)); }
  }
  window.addEventListener('velmira:synced', () => { if (!busy) summary().catch(() => {}); }, { signal });
  await load();
}

async function mountStandaloneLibrarySettings({ area, signal }) {
  const root = el('section', 'manga-studio offline-settings standalone-library-settings'); area.replaceChildren(root);
  root.append(el('h3', null, '本机书库'), el('p', 'settings-help', '从手机导入的书籍已经保存在本机，可以直接离线阅读。进度、书签和批注也会自动保存，无需再次下载。'));
  const status = el('p', 'settings-help', '正在读取本机书库…'); root.append(status);
  try {
    const { books = [] } = await apiGet('/api/books', signal);
    if (signal.aborted) return;
    status.textContent = books.length ? '本机已保存 ' + books.length + ' 本书' : '书架还是空的，返回书架点击「导入文件」即可开始。';
    for (const book of books) {
      const card = el('article', 'ai-settings-card offline-book');
      const info = el('div'); info.append(el('strong', null, book.title), el('p', 'settings-help', String(book.format || 'txt').toUpperCase() + ' · 已保存在手机'));
      const read = el('a', 'btn', '开始阅读'); read.href = '/?book=' + encodeURIComponent(book.id); card.append(info, read); root.append(card);
    }
  } catch (err) { if (!signal.aborted) status.textContent = err.message; }
  root.append(el('p', 'settings-help', '可在书籍详情中移出或恢复书籍。清除应用数据或卸载应用会删除本机书库。'));
  const connect = el('a', 'btn', '连接电脑书库（可选）'); connect.href = 'velmira://connect'; root.append(connect);
}

export async function mountStandaloneConnectionSettings({ area }) {
  const root = el('section', 'manga-studio standalone-connection-settings'); area.replaceChildren(root);
  root.append(el('h3', null, '连接电脑（可选）'),
    el('p', 'settings-help', '手机本机书库可以独立使用。如果还想阅读电脑中的书，可以连接运行 Velmira 的电脑。'),
    el('p', 'settings-help', '连接后会切换到电脑书库，在那里使用已配置的 AI、模型、联网搜索和同步功能。手机本机书库会保留；两边书库当前分别保存，不会自动合并。'));
  const connect = el('a', 'btn btn-primary', '连接电脑书库（可选）'); connect.href = 'velmira://connect'; root.append(connect);
  root.append(el('p', 'settings-help', '需要时再填写电脑地址并配对。可随时从设置返回手机本机书库。'));
}

export async function downloadBook(book, report = () => {}, signal) {
  if (!(await localStore.isDurable())) throw new Error('当前设备未启用持久存储，无法保存离线下载');
  const base = '/api/books/' + encodeURIComponent(book.id);
  const detail = await apiGet(base, signal); book = detail.book; await localStore.putBook(book);
  let imageBytes = 0;
  const image = async url => {
    imageBytes += await downloadImage(book.id, url, signal);
    if (imageBytes > 512 * 1048576) throw new Error('此书离线图片超过512MB上限');
  };
  if (book.format === 'cbz') {
    const pages = [], total = Number(book.page_count || book.chapter_count);
    if (!Number.isInteger(total) || total < 1 || total > 2000) throw new Error('漫画页数超过离线下载范围');
    await localStore.meta('download:' + book.id, { complete: false, count: 0 });
    for (let from = 0; from < total; from += 50) {
      const data = await apiGet(base + '/pages?from=' + from + '&limit=50', signal);
      if (!data.pages?.length) throw new Error('漫画页信息不完整');
      pages.push(...data.pages);
      for (const page of data.pages) { await image(page.url); report('正在下载 ' + book.title + ' · 第 ' + (page.idx + 1) + '/' + total + ' 页'); }
    }
    await localStore.meta('pages:' + book.id, { pages, count: total });
    if (pages.length !== total) throw new Error('漫画页数与书目不一致，请重试下载');
    await apiGet(base + '/progress', signal);
    await localStore.meta('download:' + book.id, { complete: true, count: pages.length, downloaded_at: Date.now() });
    return;
  }
  const toc = await apiGet(base + '/toc', signal);
  if (!toc.chapters?.length) throw new Error('书籍没有可下载的章节');
  await localStore.meta('toc:' + book.id, toc);
  let count = 0;
  await localStore.meta('download:' + book.id, { complete: false, count: 0 });
  for (const chapter of toc.chapters || []) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const data = await apiGet(base + '/chapters/' + chapter.idx, signal);
    await localStore.putChapter(book.id, chapter.idx, data);
    for (const illustration of data.illustrations || []) await image(illustration.url);
    report('正在下载 ' + book.title + ' · ' + (++count) + '/' + toc.chapters.length);
  }
  await apiGet(base + '/progress', signal);
  await apiGet(base + '/bookmarks', signal);
  for (let offset = 0; ; offset += 500) {
    const data = await apiGet(base + '/annotations?limit=500&offset=' + offset, signal);
    if ((data.annotations || []).length < 500) break;
  }
  await localStore.meta('download:' + book.id, { complete: true, count, downloaded_at: Date.now() });
}
