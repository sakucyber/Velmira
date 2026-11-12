/* ============================================================
   util/core.js —— 零依赖工具层：DOM / 存储 / 格式化 / 网络
   所有请求都支持 AbortController；错误统一为 ApiError。
   ============================================================ */

/* ---------- DOM ---------- */
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
const STORE_DEFAULTS = { book: null, chapter: 0, para: 0, offset: 0, theme: 'day', fontSize: 1 };

export const store = {
  get() {
    try {
      const raw = localStorage.getItem(STORE_KEY);
      if (!raw) return { ...STORE_DEFAULTS };
      const obj = JSON.parse(raw);
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
export const FONT_SIZES = [17, 19, 22, 25];

export function applyTheme(theme) {
  document.documentElement.dataset.theme = THEMES.some(t => t.id === theme) ? theme : 'day';
}

export function applyFontSize(index) {
  const i = clamp(Number(index) || 0, 0, FONT_SIZES.length - 1);
  document.documentElement.style.setProperty('--reader-font-size', FONT_SIZES[i] + 'px');
  return i;
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

async function request(path, { method = 'GET', body = null, signal = null, headers = null } = {}) {
  let res;
  try {
    res = await fetch(path, { method, body, signal, headers: headers || undefined, cache: 'no-store' });
  } catch (err) {
    if (err && err.name === 'AbortError') throw err;
    throw new ApiError(0, 'network_error', '网络请求失败');
  }
  const type = res.headers.get('content-type') || '';
  let data = null;
  if (type.indexOf('application/json') >= 0) {
    try { data = await res.json(); } catch (e) { data = null; }
  }
  if (!res.ok) {
    const e = data && data.error;
    throw new ApiError(res.status, e && e.code, e && e.message);
  }
  return data;
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
