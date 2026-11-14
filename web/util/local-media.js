/* Explicit offline copies of reader images. API credentials are never stored. */
let database;
function open() {
  if (database) return database;
  database = new Promise((resolve, reject) => {
    if (!globalThis.indexedDB) { reject(new Error('当前设备不支持持久离线图片')); return; }
    const request = indexedDB.open('velmira-media', 1);
    request.onupgradeneeded = () => { const store = request.result.createObjectStore('images', { keyPath: 'url' }); store.createIndex('bookId', 'bookId'); };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  return database;
}
function key(address, bookId) {
  const url = new URL(address, location.href);
  if (url.origin !== location.origin) throw new Error('仅能下载本书的同源资源');
  const prefix = '/api/books/' + encodeURIComponent(bookId) + '/';
  if (!url.pathname.startsWith(prefix) && !url.pathname.startsWith('/media/' + encodeURIComponent(bookId) + '/')) throw new Error('资源不属于此书');
  url.searchParams.delete('r'); return url.pathname + url.search;
}
export async function downloadImage(bookId, address, signal) {
  const url = key(address, bookId);
  const response = await fetch(url, { credentials: 'same-origin', cache: 'no-store', signal });
  if (!response.ok) throw new Error('图片下载失败（' + response.status + '）');
  const size = Number(response.headers.get('content-length'));
  if (size > 32 * 1048576) throw new Error('单张图片超过32MB');
  const blob = await response.blob();
  if (blob.size > 32 * 1048576 || !/^image\/(jpeg|png|webp|gif)$/i.test(blob.type)) throw new Error('图片格式或大小不符合离线下载要求');
  const db = await open();
  await new Promise((resolve, reject) => {
    const tx = db.transaction('images', 'readwrite');
    tx.objectStore('images').put({ url, bookId, blob, downloaded_at: Date.now() });
    tx.oncomplete = resolve; tx.onerror = () => reject(tx.error);
  });
  return blob.size;
}
export async function resolveImage(bookId, address) {
  try {
    const url = key(address, bookId), db = await open();
    const item = await new Promise((resolve, reject) => { const request = db.transaction('images').objectStore('images').get(url); request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error); });
    if (item?.blob) { const objectURL = URL.createObjectURL(item.blob); return { url: objectURL, release: () => URL.revokeObjectURL(objectURL) }; }
  } catch { /* Non-downloaded images continue through the authenticated service. */ }
  return { url: address, release: () => {} };
}
export async function removeImages(bookId) {
  const db = await open();
  return new Promise((resolve, reject) => {
    const tx = db.transaction('images', 'readwrite'), request = tx.objectStore('images').index('bookId').openCursor(IDBKeyRange.only(bookId));
    request.onsuccess = () => { const cursor = request.result; if (cursor) { cursor.delete(); cursor.continue(); } };
    tx.oncomplete = resolve; tx.onerror = () => reject(tx.error);
  });
}
