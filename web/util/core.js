/* ============================================================
   util/core.js —— 零依赖工具层：DOM / 存储 / 格式化 / 网络
   所有请求都支持 AbortController；错误统一为 ApiError。
   ============================================================ */

/* ---------- DOM ---------- */
import { localStore } from './local-store.js';
export function $(sel, root = document) { return root.querySelector(sel); }

export function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

export function clear(node) { if (node) node.replaceChildren(); }

export function debounce(fn, ms) {
  let timer = 0;
  const wrapped = (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
  wrapped.cancel = () => clearTimeout(timer);
  wrapped.flush = (...args) => { clearTimeout(timer); fn(...args); };
  return wrapped;
}

export function clamp(n, lo, hi) { return n < lo ? lo : (n > hi ? hi : n); }

export function intOrNull(v) {
  if (v === null || v === undefined || v === '') return null;
  const n = parseInt(v, 10);
  return Number.isInteger(n) ? n : null;
}

/* ---------- 格式化 ---------- */
export function fmtChars(n) {
  n = Number(n) || 0;
  if (n >= 10000) return (n / 10000).toFixed(1).replace(/\.0$/, '') + ' 万字';
  return n + ' 字';
}

export function fmtPercent(p) {
  p = clamp(Number(p) || 0, 0, 1);
  return (p * 100).toFixed(1) + '%';
}

/** Unix 秒 → MM-DD HH:mm（书签/注释列表用；0 显示 —） */
export function fmtTime(sec) {
  const n = Number(sec) || 0;
  if (!n) return '—';
  const d = new Date(n * 1000);
  const p2 = (v) => String(v).padStart(2, '0');
  return p2(d.getMonth() + 1) + '-' + p2(d.getDate()) + ' ' + p2(d.getHours()) + ':' + p2(d.getMinutes());
}

/* ---------- 本地存储：只存 book/chapter/para/offset/theme/fontSize ---------- */
const STORE_KEY = 'velmira.r1';
export const FONT_SIZE = { min: 12, max: 48, step: .1, default: 19 };
const STORE_DEFAULTS = { book: null, chapter: 0, para: 0, offset: 0, theme: 'day', fontSize: FONT_SIZE.default };

export const store = {
  get() {
    try {
      const raw = localStorage.getItem(STORE_KEY);
      if (!raw) return { ...STORE_DEFAULTS };
      const obj = JSON.parse(raw);
      // 旧版本保存 0–3 档位；新版保存实际像素，首次读取时保留原来的大小。
      if (obj && Number.isInteger(obj.fontSize) && obj.fontSize >= 0 && obj.fontSize <= 3) {
        obj.fontSize = [17, 19, 22, 25][obj.fontSize];
      }
      return obj && typeof obj === 'object' ? { ...STORE_DEFAULTS, ...obj } : { ...STORE_DEFAULTS };
    } catch (e) { return { ...STORE_DEFAULTS }; }
  },
  set(patch) {
    try {
      const next = { ...store.get(), ...patch };
      // 只持久化允许的字段，避免 localStorage 超 2KB
      localStorage.setItem(STORE_KEY, JSON.stringify({
        book: next.book, chapter: next.chapter, para: next.para,
        offset: next.offset, theme: next.theme, fontSize: next.fontSize,
      }));
    } catch (e) { /* 隐私模式等场景静默 */ }
  },
};

/* ---------- 主题 / 字号 ---------- */
export const THEMES = [
  { id: 'day', name: '日间', swatch: '#faf7f0' },
  { id: 'night', name: '夜间', swatch: '#191b21' },
  { id: 'sepia', name: '羊皮纸', swatch: '#f2e6cd' },
];

export function applyTheme(theme) {
  const selected = THEMES.some(t => t.id === theme) ? theme : 'day';
  document.documentElement.dataset.theme = selected;
  const color = document.querySelector('meta[name="theme-color"]');
  if (color) color.content = ({ day: '#f4f1ea', night: '#14161b', sepia: '#e9dcc2' })[selected];
  const scheme = document.querySelector('meta[name="color-scheme"]');
  if (scheme) scheme.content = selected === 'night' ? 'dark' : 'light';
}

export function normalizeFontSize(value) {
  const n = Number(value);
  return Math.round(clamp(Number.isFinite(n) ? n : FONT_SIZE.default, FONT_SIZE.min, FONT_SIZE.max) * 10) / 10;
}

export function applyFontSize(value) {
  const size = normalizeFontSize(value);
  document.documentElement.style.setProperty('--reader-font-size', size + 'px');
  return size;
}

/* ---------- 轻提示 ---------- */
let toastTimer = 0;
export function toast(msg, ms = 2000) {
  const node = document.getElementById('toast');
  if (!node) return;
  node.textContent = String(msg);
  node.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { node.hidden = true; }, ms);
}

/* ---------- 网络 ---------- */
export class ApiError extends Error {
  constructor(status, code, message) {
    super(message || code || ('HTTP ' + status));
    this.name = 'ApiError';
    this.status = status;
    this.code = code || 'http_error';
  }
}

let authState = { local: true, authenticated: true, enabled: false };
export const getAuthState = () => ({ ...authState });

/** Gate startup before requesting any book or credential metadata. Pairing
 * lives in an HttpOnly cookie, including image and EventSource requests. */
export async function bootstrapAuth() {
  const app = document.getElementById('app');
  let scannedPairingCode = '';
  if (location.hash.startsWith('#pair=')) {
    try { scannedPairingCode = decodeURIComponent(location.hash.slice(6)); } catch (_) { scannedPairingCode = ''; }
    history.replaceState(null, '', location.pathname + location.search);
  }
  let status;
  try {
    const response = await fetch('/api/auth/status', { credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error('无法读取服务访问状态');
    status = await response.json(); authState = status;
  } catch (err) {
    // Offline boot is allowed only when this origin already has reader data.
    // A cached authentication response never grants network permissions.
    if ((await localStore.listBooks().catch(() => [])).length) {
      authState = { local: false, authenticated: false, enabled: false, offline: true };
      return true;
    }
    if (app) {
      const box = el('main', 'fatal'); box.appendChild(el('p', 'fatal-msg', err.message || '无法连接服务'));
      const retry = el('button', 'btn', '重新连接'); retry.addEventListener('click', () => location.reload());
      box.appendChild(retry); app.replaceChildren(box);
    }
    return false;
  }
  if (status.local || status.authenticated) return true;
  if (!app) return false;
  const shell = app.cloneNode(true);
  const box = el('main', 'fatal'); box.style.maxWidth = '32rem'; box.style.margin = '10vh auto'; box.style.padding = '1.5rem';
  box.appendChild(el('h1', null, '连接你的 Velmira'));
  if (!status.enabled) {
    box.appendChild(el('p', 'fatal-msg', '请先在电脑端的“设置 → 局域网访问”中生成配对码并开启访问。'));
    const retry = el('button', 'btn', '刷新连接状态'); retry.addEventListener('click', () => location.reload()); box.appendChild(retry);
    app.replaceChildren(box); return false;
  }
  box.appendChild(el('p', 'manga-note', '输入电脑端生成的配对码后，即可在此设备阅读书籍和保存批注。'));
  const form = el('form'), label = el('label', 'manga-field'); label.appendChild(el('span', null, '配对码'));
  const input = el('input'); input.type = 'password'; input.autocomplete = 'off'; input.required = true;
  input.setAttribute('aria-label', '配对码'); input.spellcheck = false; input.style.width = '100%'; input.style.boxSizing = 'border-box';
  label.appendChild(input); form.appendChild(label);
  const submit = el('button', 'btn btn-primary', '配对并打开书库'); submit.type = 'submit'; form.appendChild(submit);
  const message = el('p', 'manga-note'); message.setAttribute('role', 'status'); form.appendChild(message); box.appendChild(form);
  app.replaceChildren(box);
  return new Promise(resolve => {
    const submitPairing = async event => {
      event.preventDefault(); if (submit.disabled) return; submit.disabled = true; message.textContent = '正在配对…';
      try {
        const response = await fetch('/api/auth/login', { method: 'POST', credentials: 'same-origin', cache: 'no-store',
          headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token: input.value.trim() }) });
        const data = await response.json();
        if (!response.ok) throw new ApiError(response.status, data.error?.code, data.error?.message);
        input.value = ''; authState = { ...status, authenticated: true };
        app.replaceChildren(...shell.childNodes); resolve(true);
      } catch (err) { message.textContent = err.message || '配对失败，请重试'; }
      finally { submit.disabled = false; }
    };
    form.addEventListener('submit', submitPairing);
    // A desktop-generated QR code opens the page with the pairing code in a
    // fragment. Fragments never reach the server or access logs. Consume it
    // once, then remove it from browser history before submitting.
    if (scannedPairingCode) {
      input.value = scannedPairingCode;
      scannedPairingCode = '';
      setTimeout(() => form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })), 0);
    } else input.focus();
  });
}

async function request(path, { method = 'GET', body = null, signal = null, headers = null } = {}) {
  // A reconnect does not discard the causal history of an offline draft. Keep
  // edits to that book in the same queue until sync has resolved it.
  if (method !== 'GET') {
    const route = readerRoute(path);
    if (route && ['progress', 'bookmarks', 'annotations'].includes(route.kind)) {
      const pending = await localStore.allPendingOps().catch(() => []);
      if (pending.some(op => (op.entity === 'progress' && op.entityId === route.bookId) || op.payload.book_id === route.bookId)) {
        const local = await localRequest(path, method, body).catch(error => {
          throw new ApiError(0, 'local_storage_error', '本机修改未保存：' + error.message);
        });
        if (local !== undefined) return local;
      }
    }
  }
  let res;
  try {
    res = await fetch(path, { method, body, signal, headers: headers || undefined, credentials: 'same-origin', cache: 'no-store' });
  } catch (err) {
    if (err && err.name === 'AbortError') throw err;
    const local = await localRequest(path, method, body).catch(error => {
      throw new ApiError(0, 'local_storage_error', '本机数据读写失败：' + error.message);
    });
    if (local !== undefined) return local;
    throw new ApiError(0, 'network_error', '网络请求失败');
  }
  const type = res.headers.get('content-type') || '';
  let data = null;
  if (type.indexOf('application/json') >= 0) {
    try { data = await res.json(); } catch (e) { data = null; }
  }
  if (!res.ok) {
    const e = data && data.error;
    if (res.status === 401 && e?.code === 'authentication_required') {
      window.dispatchEvent(new CustomEvent('velmira:authentication-required'));
    }
    throw new ApiError(res.status, e && e.code, e && e.message);
  }
  data = await mergePendingResponse(path, method, data).catch(() => data);
  await rememberResponse(path, method, data).catch(() => {});
  return data;
}

function readerRoute(path) {
  let url;
  try { url = new URL(path, 'http://velmira.local'); } catch { return null; }
  const match = url.pathname.match(/^\/api\/books\/([^/]+)(?:\/(chapters\/\d+|progress|toc|bookmarks|annotations|search|pages)(?:\/([^/]+))?)?$/);
  return match ? { bookId: decodeURIComponent(match[1]), kind: match[2] || 'book', id: match[3] ? decodeURIComponent(match[3]) : '', query: url.searchParams } : null;
}

async function mergePendingResponse(path, method, data) {
  if (method !== 'GET' || !data) return data;
  const route = readerRoute(path); if (!route) return data;
  const { bookId, kind, query } = route;
  const pending = await localStore.allPendingOps();
  if (kind === 'progress' && pending.some(op => op.entity === 'progress' && op.entityId === bookId)) {
    const progress = await localStore.getProgress(bookId); return progress ? { ...data, progress } : data;
  }
  const entity = kind === 'annotations' ? 'annotation' : kind === 'bookmarks' ? 'bookmark' : null;
  if (!entity) return data;
  const relevant = pending.filter(op => op.entity === entity && op.payload.book_id === bookId);
  if (!relevant.length) return data;
  const key = kind, values = new Map((data[key] || []).map(item => [item.id, item]));
  const cached = entity === 'annotation' ? await localStore.listAnnotations(bookId) : await localStore.listBookmarks(bookId);
  for (const op of relevant) {
    if (op.action === 'delete') values.delete(op.entityId);
    else {
      const value = cached.find(item => item.id === op.entityId);
      if (value && (!query.has('chapter') || value.chapter === Number(query.get('chapter')))) values.set(value.id, value);
    }
  }
  return { ...data, [key]: [...values.values()] };
}

async function rememberResponse(path, method, data) {
  if (!data) return;
  const url = new URL(path, 'http://velmira.local');
  if (method === 'GET' && url.pathname === '/api/books') {
    for (const book of data.books || []) await localStore.putBook(book);
    await localStore.meta('library:' + (url.searchParams.get('archived') || 'false'), data);
    return;
  }
  const route = readerRoute(path); if (!route) return;
  const { bookId, kind, id } = route;
  if (method === 'GET' && kind === 'book' && data.book) await localStore.putBook(data.book);
  else if (method === 'GET' && kind.startsWith('chapters/')) await localStore.putChapter(bookId, Number(kind.split('/')[1]), data);
  else if (kind === 'progress' && data.progress) await localStore.putProgress(bookId, data.progress);
  else if (method === 'GET' && kind === 'toc') await localStore.meta('toc:' + bookId, data);
  else if (kind === 'bookmarks') {
    for (const item of data.bookmarks || (data.bookmark ? [data.bookmark] : [])) await localStore.putBookmark({ ...item, book_id: bookId });
    if (method === 'DELETE' && id) await localStore.deleteBookmark(id);
  } else if (kind === 'annotations') {
    for (const item of data.annotations || (data.annotation ? [data.annotation] : [])) await localStore.putAnnotation({ ...item, book_id: bookId });
    if (method === 'DELETE' && id) await localStore.deleteAnnotation(id);
  }
}

async function localRequest(path, method, body) {
  const url = new URL(path, 'http://velmira.local');
  if (method === 'GET' && url.pathname === '/api/books') {
    const cached = await localStore.meta('library:' + (url.searchParams.get('archived') || 'false'));
    if (cached) return { ...cached, offline: true };
    const books = await localStore.listBooks();
    return books.length ? { books, offline: true } : undefined;
  }
  const route = readerRoute(path); if (!route) return undefined;
  const { bookId, kind, id, query } = route;
  let payload = {};
  if (body) { try { payload = JSON.parse(body); } catch { return undefined; } }
  if (method === 'GET') {
    if (kind === 'book') { const book = await localStore.getBook(bookId); return book ? { book, offline: true } : undefined; }
    if (kind.startsWith('chapters/')) return (await localStore.getChapter(bookId, Number(kind.split('/')[1]))) || undefined;
    if (kind === 'toc') return (await localStore.meta('toc:' + bookId)) || undefined;
    if (kind === 'pages') {
      const cached = await localStore.meta('pages:' + bookId); if (!cached) return undefined;
      if (id) return cached.pages.find(page => page.idx === Number(id));
      const from = Math.max(0, Number(query.get('from')) || 0), limit = Math.max(1, Math.min(50, Number(query.get('limit')) || 12));
      return { pages: cached.pages.slice(from, from + limit), count: cached.count, from, offline: true };
    }
    if (kind === 'progress') { const progress = await localStore.getProgress(bookId); return { progress: progress || { chapter: 0, para: 0, offset: 0, percent: 0 } }; }
    if (kind === 'bookmarks') return { bookmarks: await localStore.listBookmarks(bookId), offline: true };
    if (kind === 'annotations') {
      let annotations = await localStore.listAnnotations(bookId);
      if (query.has('chapter')) annotations = annotations.filter(item => item.chapter === Number(query.get('chapter')));
      const offset = Math.max(0, Number(query.get('offset')) || 0), limit = Math.min(500, Math.max(1, Number(query.get('limit')) || 200));
      return { annotations: annotations.sort((a, b) => a.chapter - b.chapter || a.para - b.para || a.start_offset - b.start_offset).slice(offset, offset + limit), offline: true };
    }
    if (kind === 'search') {
      const q = (query.get('q') || '').trim(), hits = [];
      if (!q) return { hits, offline: true, elapsed_ms: 0 };
      const limit = Math.max(1, Math.min(50, Number(query.get('limit')) || 20));
      const chapters = (await localStore.listChapters(bookId)).sort((a, b) => a.idx - b.idx);
      const escape = value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
      for (const chapter of chapters) {
        for (let para = 0; para < (chapter.paragraphs || []).length; para++) {
          const text = chapter.paragraphs[para], index = text.indexOf(q);
          if (index < 0) continue;
          hits.push({ chapter: chapter.idx, para, offset: Array.from(text.slice(0, index)).length, snippet: escape(text.slice(Math.max(0, index - 45), index)) + '<b>' + escape(q) + '</b>' + escape(text.slice(index + q.length, index + q.length + 65)) });
          if (hits.length >= limit) return { hits, offline: true, elapsed_ms: 0 };
        }
      }
      return { hits, offline: true, elapsed_ms: 0 };
    }
    return undefined;
  }
  if (kind === 'progress' && method === 'PUT') {
    const previous = await localStore.getProgress(bookId), chapter = await localStore.getChapter(bookId, payload.chapter);
    const toc = await localStore.meta('toc:' + bookId), chapters = toc?.chapters || [];
    const total = chapters.reduce((sum, item) => sum + (Number(item.chars) || 0), 0);
    const before = chapters.filter(item => item.idx < payload.chapter).reduce((sum, item) => sum + (Number(item.chars) || 0), 0);
    const consumed = (chapter?.paragraphs || []).slice(0, payload.para || 0).reduce((sum, text) => sum + Array.from(text).length, 0) + (payload.offset || 0);
    const book = await localStore.getBook(bookId);
    const percent = book?.format === 'cbz' && book.chapter_count ? Math.min(1, (payload.chapter + 1) / book.chapter_count)
      : total ? Math.min(1, (before + consumed) / total) : previous?.percent || 0;
    const progress = await localStore.commitLocalEdit('progress', 'upsert', bookId, { ...payload, percent, updated_at: Math.floor(Date.now() / 1000) });
    return { progress, offline: true };
  }
  if (kind === 'bookmarks') {
    if (method === 'POST') {
      const bookmark = await localStore.commitLocalEdit('bookmark', 'upsert', null, { ...payload, book_id: bookId, created_at: Math.floor(Date.now() / 1000) });
      return { bookmark, offline: true };
    }
    if (method === 'DELETE' && id) {
      await localStore.commitLocalEdit('bookmark', 'delete', id, { book_id: bookId }); return { ok: true, offline: true };
    }
  }
  if (kind === 'annotations') {
    if (method === 'POST') {
      const now = Math.floor(Date.now() / 1000);
      const annotation = await localStore.commitLocalEdit('annotation', 'upsert', null, { ...payload, book_id: bookId, created_at: now, updated_at: now });
      return { annotation, offline: true };
    }
    if (method === 'PATCH' && id) {
      const existing = (await localStore.listAnnotations(bookId)).find(item => item.id === id);
      if (!existing) return undefined;
      const annotation = await localStore.commitLocalEdit('annotation', 'upsert', id, { ...payload, book_id: bookId, updated_at: Math.floor(Date.now() / 1000) });
      return { annotation, offline: true };
    }
    if (method === 'DELETE' && id) {
      await localStore.commitLocalEdit('annotation', 'delete', id, { book_id: bookId }); return { ok: true, offline: true };
    }
  }
  return undefined;
}

export const apiGet = (path, signal) => request(path, { signal });

export const apiPut = (path, body, signal) => request(path, {
  method: 'PUT', body: JSON.stringify(body), signal,
  headers: { 'Content-Type': 'application/json' },
});

export const apiPost = (path, body, signal) => request(path, {
  method: 'POST', body: JSON.stringify(body), signal,
  headers: { 'Content-Type': 'application/json' },
});

export const apiPatch = (path, body, signal) => request(path, {
  method: 'PATCH', body: JSON.stringify(body), signal,
  headers: { 'Content-Type': 'application/json' },
});

export const apiDelete = (path, signal) => request(path, { method: 'DELETE', signal });

/* 上传 TXT：raw octet-stream（免 multipart 依赖），用 XHR 拿上传进度 */
export function uploadBook(file, onProgress, signal) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/books?filename=' + encodeURIComponent(file.name || 'book.txt'));
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.responseType = 'text';
    xhr.upload.onprogress = (e) => {
      if (onProgress) onProgress(e.lengthComputable ? e.loaded / Math.max(1, e.total) : 0);
    };
    xhr.onload = () => {
      let data = null;
      try { data = JSON.parse(xhr.responseText); } catch (e) { data = null; }
      if (xhr.status >= 200 && xhr.status < 300 && data) resolve(data);
      else {
        const e = data && data.error;
        reject(new ApiError(xhr.status, e && e.code, e && e.message));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, 'network_error', '上传失败'));
    xhr.onabort = () => reject(new DOMException('Aborted', 'AbortError'));
    if (signal) signal.addEventListener('abort', () => xhr.abort(), { once: true });
    xhr.send(file);
  });
}
