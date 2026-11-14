/*
 * Local-first storage for the reader.
 *
 * The service remains the source for AI credentials and provider settings. This
 * module intentionally stores only reader data and an operation queue. It uses
 * IndexedDB in browsers and a small in-memory adapter when storage is disabled,
 * so private browsing and older WebViews still keep the reader usable.
 */

const DB_NAME = 'velmira-local';
const DB_VERSION = 1;
const STORE_NAMES = ['books', 'chapters', 'progress', 'bookmarks', 'annotations', 'ops', 'meta'];
let databasePromise = null;
let activeSync = null;
const memory = Object.fromEntries(STORE_NAMES.map(name => [name, new Map()]));
const SYNC_FIELDS = {
  progress: ['chapter', 'para', 'offset', 'percent', 'updated_at'],
  bookmark: ['book_id', 'chapter', 'para', 'offset', 'label', 'created_at'],
  annotation: ['book_id', 'chapter', 'para', 'start_offset', 'end_offset', 'text', 'note', 'color', 'created_at', 'updated_at'],
};

function clone(value) {
  if (value === undefined || value === null) return value;
  try { return structuredClone(value); } catch { return JSON.parse(JSON.stringify(value)); }
}

function randomId(prefix) {
  try { return prefix + crypto.randomUUID(); } catch {
    return prefix + Date.now().toString(36) + Math.random().toString(36).slice(2);
  }
}

function getDeviceId() {
  const key = 'velmira.device-id';
  try {
    const existing = localStorage.getItem(key);
    if (existing && /^[A-Za-z0-9_-]{8,100}$/.test(existing)) return existing;
    const value = randomId('device_');
    localStorage.setItem(key, value);
    return value;
  } catch { return randomId('session_'); }
}

export let deviceId = getDeviceId();
let sequence = 0;
try { sequence = Number(localStorage.getItem('velmira.device-seq')) || 0; } catch { /* private mode */ }
const ENTITY_STORES = { progress: 'progress', bookmark: 'bookmarks', annotation: 'annotations' };
const QUEUE_READS = {
  operations: { store: 'ops', index: 'status', query: 'pending', all: true },
  sequence: { store: 'meta', key: 'device-seq' },
  identity: { store: 'meta', key: 'device-id' },
};

function valueKey(value) { return value.id ?? value.key ?? value.bookId ?? value.opId; }

// All reads and writes belong to one transaction. In particular, a quota error
// must roll back both the reader entity and its outbox entry. The planner is
// synchronous so IndexedDB cannot auto-commit between its reads and writes.
async function atomicWrite(stores, reads, plan) {
  const db = await openDatabase(), entries = Object.entries(reads);
  if (!db) {
    const values = Object.fromEntries(entries.map(([key, read]) => {
      let value;
      if (read.all) value = [...memoryStore(read.store).values()].filter(item => !read.index || item[read.index] === read.query);
      else value = memoryStore(read.store).get(read.key);
      return [key, clone(value)];
    }));
    const writes = [], result = plan(values, {
      put: (store, value) => writes.push({ store, value: ensurePublic(value) }),
      delete: (store, key) => writes.push({ store, key, remove: true }),
    });
    for (const write of writes) {
      if (write.remove) memoryStore(write.store).delete(write.key);
      else memoryStore(write.store).set(valueKey(write.value), write.value);
    }
    return result;
  }
  return new Promise((resolve, reject) => {
    const tx = db.transaction([...new Set(stores)], 'readwrite'), values = {};
    let result, failure, remaining = entries.length;
    const abort = error => { failure = error; try { tx.abort(); } catch { reject(error); } };
    const write = {
      put: (store, value) => tx.objectStore(store).put(ensurePublic(value)),
      delete: (store, key) => tx.objectStore(store).delete(key),
    };
    const finish = () => { try { result = plan(values, write); } catch (error) { abort(error); } };
    tx.oncomplete = () => resolve(result);
    tx.onerror = event => { failure ||= event.target?.error || tx.error; };
    tx.onabort = () => reject(failure || tx.error || new Error('本地写入已回滚，请检查设备剩余空间'));
    try {
      for (const [key, read] of entries) {
        const source = read.index ? tx.objectStore(read.store).index(read.index) : tx.objectStore(read.store);
        const request = read.all ? source.getAll(read.query) : source.get(read.key);
        request.onsuccess = () => { values[key] = request.result; if (--remaining === 0) finish(); };
      }
      if (!remaining) finish();
    } catch (error) { abort(error); }
  });
}

function adoptIdentity(id, seq) {
  deviceId = id; localStore.deviceId = id; sequence = seq;
  // IndexedDB is authoritative. If localStorage fails after commit, the next
  // transaction/sync restores this identity from the committed metadata.
  try { localStorage.setItem('velmira.device-id', id); localStorage.setItem('velmira.device-seq', String(seq)); } catch { /* private mode */ }
}

function queueIdentity(rows) {
  const id = rows.identity?.value || deviceId;
  return { id, seq: Math.max(Number(rows.sequence?.value) || 0, id === deviceId ? sequence : 0) };
}

function makeOperation(entity, action, entityId, payload, baseRevision = null) {
  const allowed = SYNC_FIELDS[entity];
  if (!allowed || !['upsert', 'delete'].includes(action)) throw new Error('不支持的同步数据');
  const safe = ensurePublic(Object.fromEntries(allowed.filter(key => payload?.[key] !== undefined).map(key => [key, payload[key]])));
  return {
    opId: randomId(deviceId + ':'), deviceId, entity, action,
    entityId: entityId == null ? null : String(entityId), payload: safe,
    baseRevision: baseRevision == null ? (payload?.sync_revision == null ? null : Number(payload.sync_revision)) : Number(baseRevision),
    createdAt: Date.now(), status: 'pending',
  };
}

function localEntity(entity, id, payload, previous) {
  const safe = ensurePublic(payload);
  if (entity === 'progress') return { ...previous, ...safe, bookId: id, updatedAt: Date.now() };
  const bookId = safe.book_id || previous?.book_id;
  if (!bookId) throw new Error('本地记录缺少书籍编号');
  return { ...previous, ...safe, book_id: bookId, bookId, id, updatedAt: Date.now() };
}

async function enqueueAtomic(value, mutation = null) {
  const store = ENTITY_STORES[value.entity];
  const saved = await atomicWrite(['meta', 'ops', ...(mutation ? [store] : [])], {
    ...QUEUE_READS, ...(mutation ? { previous: { store, key: value.entityId } } : {}),
  }, (rows, write) => {
    const identity = queueIdentity(rows);
    const entityValue = mutation && value.action !== 'delete' ? localEntity(value.entity, value.entityId, mutation.payload, rows.previous) : null;
    const operation = mutation ? makeOperation(value.entity, value.action, value.entityId, entityValue || mutation.payload,
      value.baseRevision ?? rows.previous?.sync_revision ?? null) : value;
    const existing = rows.operations.find(op => !op.transmitted && op.entity === operation.entity && op.entityId === operation.entityId);
    const queued = existing ? { ...existing, action: operation.action, payload: operation.payload }
      : { ...operation, deviceId: identity.id, opId: randomId(identity.id + ':'), seq: ++identity.seq };
    if (mutation) {
      if (operation.action === 'delete') write.delete(store, operation.entityId);
      else write.put(store, entityValue);
    }
    write.put('ops', queued);
    write.put('meta', { key: 'device-id', value: identity.id });
    write.put('meta', { key: 'device-seq', value: identity.seq });
    return { operation: queued, entityValue, identity };
  });
  adoptIdentity(saved.identity.id, saved.identity.seq);
  return saved;
}

function hasCredentialKey(value, seen = new Set()) {
  if (!value || typeof value !== 'object' || seen.has(value)) return false;
  seen.add(value);
  if (Array.isArray(value)) return value.some(item => hasCredentialKey(item, seen));
  return Object.entries(value).some(([key, child]) =>
    /(?:api[_-]?key|access[_-]?token|refresh[_-]?token|password|secret|authorization)/i.test(key)
      || hasCredentialKey(child, seen));
}

function ensurePublic(value) {
  if (hasCredentialKey(value)) throw new Error('本地同步数据不能包含密钥或令牌');
  return clone(value);
}

function openDatabase() {
  if (databasePromise) return databasePromise;
  if (typeof indexedDB === 'undefined') return Promise.resolve(null);
  databasePromise = new Promise((resolve) => {
    let request;
    try { request = indexedDB.open(DB_NAME, DB_VERSION); } catch { resolve(null); return; }
    request.onupgradeneeded = () => {
      const db = request.result;
      const books = db.createObjectStore('books', { keyPath: 'id' });
      books.createIndex('updatedAt', 'updatedAt');
      const chapters = db.createObjectStore('chapters', { keyPath: 'key' });
      chapters.createIndex('bookId', 'bookId');
      chapters.createIndex('bookChapter', ['bookId', 'idx'], { unique: true });
      const progress = db.createObjectStore('progress', { keyPath: 'bookId' });
      progress.createIndex('updatedAt', 'updatedAt');
      const bookmarks = db.createObjectStore('bookmarks', { keyPath: 'id' });
      bookmarks.createIndex('bookId', 'bookId');
      const annotations = db.createObjectStore('annotations', { keyPath: 'id' });
      annotations.createIndex('bookId', 'bookId');
      const ops = db.createObjectStore('ops', { keyPath: 'opId' });
      ops.createIndex('status', 'status');
      ops.createIndex('entity', ['entity', 'entityId']);
      db.createObjectStore('meta', { keyPath: 'key' });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(null);
    request.onblocked = () => resolve(null);
  });
  return databasePromise;
}

function memoryStore(name) { return memory[name]; }

async function put(name, value, key) {
  const db = await openDatabase();
  const safe = ensurePublic(value);
  if (!db) { memoryStore(name).set(key ?? value.id ?? value.key ?? value.bookId ?? value.opId, safe); return safe; }
  return new Promise((resolve, reject) => {
    const tx = db.transaction(name, 'readwrite');
    tx.objectStore(name).put(safe);
    tx.oncomplete = () => resolve(safe);
    tx.onerror = () => reject(tx.error || new Error('本地存储失败'));
    tx.onabort = () => reject(tx.error || new Error('本地存储已回滚'));
  });
}

async function get(name, key) {
  const db = await openDatabase();
  if (!db) return clone(memoryStore(name).get(key));
  return new Promise((resolve, reject) => {
    const tx = db.transaction(name, 'readonly');
    const request = tx.objectStore(name).get(key);
    request.onsuccess = () => resolve(clone(request.result));
    request.onerror = () => reject(request.error || new Error('本地读取失败'));
  });
}

async function getByIndex(name, index, query) {
  const db = await openDatabase();
  if (!db) return [...memoryStore(name).values()].filter(item => {
    const value = Array.isArray(index) ? index.map(key => item[key]) : item[index];
    return Array.isArray(query) ? JSON.stringify(value) === JSON.stringify(query) : value === query;
  }).map(clone);
  return new Promise((resolve, reject) => {
    const tx = db.transaction(name, 'readonly');
    const request = tx.objectStore(name).index(index).getAll(query);
    request.onsuccess = () => resolve((request.result || []).map(clone));
    request.onerror = () => reject(request.error || new Error('本地读取失败'));
  });
}

async function remove(name, key) {
  const db = await openDatabase();
  if (!db) { memoryStore(name).delete(key); return; }
  return new Promise((resolve, reject) => {
    const tx = db.transaction(name, 'readwrite');
    tx.objectStore(name).delete(key);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error || new Error('本地删除失败'));
    tx.onabort = () => reject(tx.error || new Error('本地删除已回滚'));
  });
}

export const localStore = {
  deviceId,
  async isDurable() {
    const db = await openDatabase();
    if (db) {
      const id = await this.meta('device-id');
      if (id) adoptIdentity(id, Number(await this.meta('device-seq')) || 0);
    }
    return !!db;
  },

  async putBook(book) {
    if (!book || !book.id) return null;
    const previous = await get('books', book.id);
    return put('books', { ...previous, ...ensurePublic(book), updatedAt: Date.now() });
  },
  getBook(bookId) { return get('books', bookId); },

  async putChapter(bookId, idx, chapter) {
    if (!bookId || !Number.isInteger(Number(idx)) || !chapter) return null;
    return put('chapters', { ...ensurePublic(chapter), key: bookId + ':' + Number(idx), bookId, idx: Number(idx), cachedAt: Date.now() });
  },
  getChapter(bookId, idx) { return get('chapters', bookId + ':' + Number(idx)); },
  listChapters(bookId) { return getByIndex('chapters', 'bookId', bookId); },

  async putProgress(bookId, progress) {
    if (!bookId || !progress) return null;
    const previous = await get('progress', bookId);
    return put('progress', { ...(previous?.sync_revision == null ? {} : { sync_revision: previous.sync_revision }), ...ensurePublic(progress), bookId, updatedAt: Date.now() });
  },
  getProgress(bookId) { return get('progress', bookId); },

  async putBookmark(bookmark) {
    if (!bookmark || !bookmark.book_id) return null;
    const previous = bookmark.id ? await get('bookmarks', bookmark.id) : null;
    const value = { ...(previous?.sync_revision == null ? {} : { sync_revision: previous.sync_revision }), ...ensurePublic(bookmark), bookId: bookmark.book_id, id: bookmark.id || randomId('local_bm_'), updatedAt: Date.now() };
    return put('bookmarks', value);
  },
  deleteBookmark(id) { return remove('bookmarks', id); },
  listBookmarks(bookId) { return getByIndex('bookmarks', 'bookId', bookId); },

  async putAnnotation(annotation) {
    if (!annotation || !annotation.book_id) return null;
    const previous = annotation.id ? await get('annotations', annotation.id) : null;
    const value = { ...(previous?.sync_revision == null ? {} : { sync_revision: previous.sync_revision }), ...ensurePublic(annotation), bookId: annotation.book_id, id: annotation.id || randomId('local_ann_'), updatedAt: Date.now() };
    return put('annotations', value);
  },
  deleteAnnotation(id) { return remove('annotations', id); },
  listAnnotations(bookId) { return getByIndex('annotations', 'bookId', bookId); },

  async enqueue(entity, action, entityId, payload, baseRevision = null) {
    return (await enqueueAtomic(makeOperation(entity, action, entityId, payload, baseRevision))).operation;
  },
  async commitLocalEdit(entity, action, entityId, payload, baseRevision = null) {
    // The entity, operation and sequence either all commit or all roll back.
    const safe = ensurePublic(payload || {});
    const id = entityId || safe.id || (entity === 'annotation' ? randomId('local_ann_') : entity === 'bookmark' ? randomId('local_bm_') : null);
    if (!id) throw new Error('本地修改缺少记录编号');
    return (await enqueueAtomic(makeOperation(entity, action, id, safe, baseRevision), { payload: safe })).entityValue;
  },
  async pendingOps(limit = 100) {
    return (await this.allPendingOps()).slice(0, Math.max(1, Math.min(500, limit)));
  },
  async allPendingOps() {
    const db = await openDatabase();
    if (!db) return [...memoryStore('ops').values()].filter(item => item.status === 'pending').sort((a, b) => a.seq - b.seq).map(clone);
    const values = await getByIndex('ops', 'status', 'pending');
    return values.sort((a, b) => a.seq - b.seq);
  },
  async acknowledge(opIds) {
    for (const opId of opIds || []) {
      const op = await get('ops', opId);
      if (op) await put('ops', { ...op, status: 'acked', ackedAt: Date.now() });
    }
  },
  async syncBatch(limit = 100) {
    return { device_id: deviceId, operations: (await this.pendingOps(limit)).map(op => ({
      op_id: op.opId, seq: op.seq, entity: op.entity, entity_id: op.entityId, action: op.action,
      payload: op.payload, ...(op.baseRevision == null ? {} : { base_revision: op.baseRevision }),
    })) };
  },
  async listBooks() {
    const db = await openDatabase();
    if (!db) return [...memoryStore('books').values()].map(clone);
    return new Promise((resolve, reject) => {
      const tx = db.transaction('books', 'readonly'), request = tx.objectStore('books').getAll();
      request.onsuccess = () => resolve((request.result || []).map(clone)); request.onerror = () => reject(request.error);
    });
  },
  async meta(key, value = undefined) {
    if (value === undefined) return (await get('meta', key))?.value;
    await put('meta', { key, value: ensurePublic(value) }); return value;
  },
  async removeBook(bookId) {
    const pending = await this.allPendingOps();
    if (pending.some(op => (op.entity === 'progress' && op.entityId === bookId) || op.payload.book_id === bookId)) {
      throw new Error('这本书有尚未同步的修改，请先同步或处理冲突');
    }
    for (const chapter of await this.listChapters(bookId)) await remove('chapters', chapter.key);
    for (const bookmark of await this.listBookmarks(bookId)) await this.deleteBookmark(bookmark.id);
    for (const annotation of await this.listAnnotations(bookId)) await this.deleteAnnotation(annotation.id);
    await remove('progress', bookId); await remove('books', bookId);
    for (const key of ['download:', 'toc:', 'pages:']) await remove('meta', key + bookId);
    for (const key of ['library:false', 'library:true']) {
      const library = await this.meta(key);
      if (library?.books) await this.meta(key, { ...library, books: library.books.filter(book => book.id !== bookId) });
    }
  },
  async resolveConflict(opId, choice) {
    if (activeSync) await activeSync.catch(() => {});
    const run = () => this._resolveConflict(opId, choice);
    return globalThis.navigator?.locks?.request ? navigator.locks.request('velmira-device-sync', run) : run();
  },
  async _resolveConflict(opId, choice) {
    if (!['server', 'local'].includes(choice)) throw new Error('请选择保留本机或服务端版本');
    const operation = await get('ops', opId);
    if (!operation) throw new Error('冲突操作已不存在');
    const store = ENTITY_STORES[operation.entity];
    const saved = await atomicWrite(['ops', 'meta', store], {
      ...QUEUE_READS, previous: { store, key: operation.entityId }, conflicts: { store: 'meta', key: 'sync:conflicts' },
    }, (rows, write) => {
      const conflicts = rows.conflicts?.value || [], conflict = conflicts.find(item => item.op_id === opId);
      if (!conflict || !rows.operations.some(op => op.opId === opId)) throw new Error('冲突已处理');
      const identity = queueIdentity(rows);
      const related = rows.operations.filter(op => op.entity === operation.entity && op.entityId === operation.entityId).sort((a, b) => a.seq - b.seq);
      const relatedIds = new Set(related.map(op => op.opId)), latest = related.at(-1);
      for (const op of related) write.put('ops', { ...op, status: 'acked', ackedAt: Date.now() });
      if (choice === 'server') write.delete(store, operation.entityId);
      else {
        // Edits made after a conflict appeared are part of the same draft.
        // Keep the latest local payload when replacing that causal history.
        const payload = { ...latest.payload };
        if (['progress', 'annotation'].includes(operation.entity)) payload.updated_at = Math.max(Math.floor(Date.now() / 1000), Number(conflict.current_updated_at || 0) + 1);
        const id = conflict.reason === 'deleted' && latest.action !== 'delete' && operation.entity !== 'progress'
          ? randomId(operation.entity === 'annotation' ? 'local_ann_' : 'local_bm_') : operation.entityId;
        const revision = id === operation.entityId ? conflict.revision ?? null : 0;
        const replacement = { ...makeOperation(operation.entity, latest.action, id, payload, revision),
          deviceId: identity.id, opId: randomId(identity.id + ':'), seq: ++identity.seq };
        write.put('ops', replacement);
        if (latest.action === 'delete' || id !== operation.entityId) write.delete(store, operation.entityId);
        if (latest.action !== 'delete') write.put(store, localEntity(operation.entity, id, { ...payload, sync_revision: revision }, rows.previous));
      }
      write.put('meta', { key: 'device-id', value: identity.id });
      write.put('meta', { key: 'device-seq', value: identity.seq });
      write.put('meta', { key: 'sync:conflicts', value: conflicts.filter(item => !relatedIds.has(item.op_id)) });
      return identity;
    });
    adoptIdentity(saved.id, saved.seq);
  },
  async resetDevice() {
    if (activeSync) await activeSync.catch(() => {});
    const run = () => this._resetDevice();
    return globalThis.navigator?.locks?.request ? navigator.locks.request('velmira-device-sync', run) : run();
  },
  async _resetDevice() {
    const saved = await atomicWrite(['ops', 'meta'], {
      operations: QUEUE_READS.operations, conflicts: { store: 'meta', key: 'sync:conflicts' },
    }, (rows, write) => {
      const id = randomId('device_'), conflicts = [], previous = new Map((rows.conflicts?.value || []).map(item => [item.op_id, item]));
      let seq = 0;
      for (const operation of rows.operations.sort((a, b) => a.seq - b.seq)) {
        const { transmitted, ...original } = operation;
        const replacement = { ...original, opId: randomId(id + ':'), deviceId: id, seq: ++seq, createdAt: Date.now() };
        write.put('ops', { ...operation, status: 'acked', ackedAt: Date.now() });
        write.put('ops', replacement);
        if (previous.has(operation.opId)) conflicts.push({ ...previous.get(operation.opId), op_id: replacement.opId });
      }
      write.put('meta', { key: 'device-id', value: id });
      write.put('meta', { key: 'device-seq', value: seq });
      write.put('meta', { key: 'sync:cursor', value: 0 });
      write.put('meta', { key: 'sync:conflicts', value: conflicts });
      return { id, seq };
    });
    adoptIdentity(saved.id, saved.seq);
    return saved.id;
  },
  async sync(options = {}) {
    if (activeSync) return activeSync;
    const run = () => this._sync(options);
    activeSync = (globalThis.navigator?.locks?.request
      ? navigator.locks.request('velmira-device-sync', run) : run()).finally(() => { activeSync = null; });
    return activeSync;
  },
  async _sync({ signal } = {}) {
    const currentId = await this.meta('device-id');
    if (currentId) adoptIdentity(currentId, Number(await this.meta('device-seq')) || 0);
    const request = async (path, body) => {
      const response = await fetch(path, { method: body === undefined ? 'GET' : 'POST', credentials: 'same-origin', cache: 'no-store', signal,
        headers: body === undefined ? undefined : { 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
      const value = await response.json();
      if (!response.ok) {
        const error = new Error(value.error?.message || '同步失败'); error.status = response.status; error.code = value.error?.code;
        if (response.status === 401 && error.code === 'authentication_required' && typeof window !== 'undefined') {
          window.dispatchEvent(new CustomEvent('velmira:authentication-required'));
        }
        throw error;
      }
      return value;
    };
    const registered = await request('/api/sync/device', { device_id: deviceId, label: globalThis.navigator?.platform || 'Browser' });
    const identity = await atomicWrite(['meta', 'ops'], QUEUE_READS, (rows, write) => {
      const current = queueIdentity(rows);
      current.seq = Math.max(current.seq, Number(registered.device?.last_seq) || 0);
      write.put('meta', { key: 'device-id', value: current.id });
      write.put('meta', { key: 'device-seq', value: current.seq });
      return current;
    });
    adoptIdentity(identity.id, identity.seq);
    const result = { accepted: [], duplicates: [], conflicts: [] };
    const conflictMap = new Map((await this.meta('sync:conflicts') || []).map(item => [item.op_id, item]));
    while (true) {
      const candidates = (await this.allPendingOps()).filter(op => !conflictMap.has(op.opId));
      if (!candidates.length) break;
      const chunk = candidates.slice(0, 100);
      for (const operation of chunk) await put('ops', { ...operation, transmitted: true });
      const response = await request('/api/sync/push', { device_id: deviceId, operations: chunk.map(op => ({
        op_id: op.opId, seq: op.seq, entity: op.entity, entity_id: op.entityId, action: op.action, payload: op.payload,
        ...(op.baseRevision == null ? {} : { base_revision: op.baseRevision }),
      })) });
      const ids = [...(response.accepted || []), ...(response.duplicates || [])].map(item => typeof item === 'string' ? item : item.op_id).filter(Boolean);
      await this.acknowledge(ids);
      result.accepted.push(...response.accepted || []); result.duplicates.push(...response.duplicates || []);
      result.conflicts.push(...response.conflicts || []);
      for (const conflict of response.conflicts || []) conflictMap.set(conflict.op_id, conflict);
      if (!ids.length && !response.conflicts?.length) throw new Error('同步没有确认操作，请重试');
    }
    // Preserve failed concurrent edits for a settings screen to display. A
    // conflict is never silently acknowledged or replaced by remote data.
    await this.meta('sync:conflicts', [...conflictMap.values()]);
    const isPending = async (entity, id) => (await this.allPendingOps()).some(op => op.entity === entity && op.entityId === id);
    let cursor = Number(await this.meta('sync:cursor')) || 0, pulled = 0, more = true;
    const apply = async operation => {
      const { entity, entity_id: id, action, payload = {} } = operation;
      if (await isPending(entity, id)) return;
      const value = { ...payload, sync_revision: operation.cursor };
      if (entity === 'progress') {
        if (action === 'delete') await remove('progress', id); else await this.putProgress(id, value);
      } else if (entity === 'bookmark') {
        if (action === 'delete') await this.deleteBookmark(id); else await this.putBookmark({ ...value, id });
      } else if (entity === 'annotation') {
        if (action === 'delete') await this.deleteAnnotation(id); else await this.putAnnotation({ ...value, id });
      }
    };
    while (more) {
      const page = await request('/api/sync/pull?device_id=' + encodeURIComponent(deviceId) + '&cursor=' + cursor + '&limit=100');
      for (const operation of page.operations || []) { await apply(operation); pulled += 1; }
      cursor = Number(page.cursor) || cursor; more = !!page.has_more;
      await this.meta('sync:cursor', cursor);
    }
    // Snapshot includes current book metadata and covers entities written before
    // the operation protocol existed. Pending local edits remain untouched.
    let snapshotOffset = 0, snapshotMore = true, snapshotCursor = null;
    while (snapshotMore) {
      const snapshot = await request('/api/sync/snapshot?offset=' + snapshotOffset + '&limit=500');
      if (snapshotCursor === null) snapshotCursor = Number(snapshot.cursor) || 0;
      for (const book of snapshot.books || []) await this.putBook(book);
      for (const value of snapshot.progress || []) if (!await isPending('progress', value.book_id)) await this.putProgress(value.book_id, value);
      for (const value of snapshot.bookmarks || []) if (!await isPending('bookmark', value.id)) await this.putBookmark(value);
      for (const value of snapshot.annotations || []) if (!await isPending('annotation', value.id)) await this.putAnnotation(value);
      snapshotMore = !!snapshot.has_more;
      snapshotOffset = Number(snapshot.next_offset) || snapshotOffset + 500;
    }
    // Paging snapshots can overlap concurrent edits. Replay everything newer
    // than the first page's cursor to converge without skipping an operation.
    let replayCursor = snapshotCursor || 0, replayMore = true;
    while (replayMore) {
      const page = await request('/api/sync/pull?device_id=' + encodeURIComponent(deviceId) + '&cursor=' + replayCursor + '&limit=100');
      for (const operation of page.operations || []) { await apply(operation); pulled += 1; }
      replayCursor = Number(page.cursor) || replayCursor; replayMore = !!page.has_more;
    }
    cursor = Math.max(cursor, replayCursor); await this.meta('sync:cursor', cursor);
    await this.meta('sync:last', { at: Date.now(), cursor, pulled });
    return { ...result, pulled, cursor };
  },
};

export async function cacheResponse(bookId, kind, value) {
  if (kind === 'book') return localStore.putBook(value?.book || value);
  if (kind === 'chapter') return localStore.putChapter(bookId, value?.idx, value);
  if (kind === 'progress') return localStore.putProgress(bookId, value?.progress || value);
  return null;
}
