// The page's side of the reader's caches (reader design, "Caching"). The
// page and the worker share CacheStorage, so wiping and evicting happen
// here, where the answers are classified; the worker only fills them.
import { enc, encPath } from './api.js';

// The owner record (read-owner, under `ownerKey`) is kept by the worker
// (sw.js, `takeOver`); the page only reads it and deletes it with the rest.
const { content, meta, owner, ownerKey } = self.READ_CACHES;
// How long to wait for a worker to confirm a mark before deleting anyway.
const MARK_WAIT_MS = 1000;
// Without Cache Storage (an unusual private mode) the reader works online
// and keeps nothing.
const CACHING = 'caches' in self;

// Runs `work` against Cache Storage. When storage is missing or refuses
// (denied by browser settings, a full disk), it logs and returns `fallback`
// instead of throwing, so boot, reads and deletion never fail on it.
async function storage(work, fallback = undefined) {
    if (!CACHING) return fallback;
    try {
        return await work();
    } catch (error) {
        console.warn('reader cache unavailable:', error?.name);
        return fallback;
    }
}

// The workers that may be answering reads: the one controlling this page,
// and the registration's active worker. They differ while a new build takes
// over (it activates, then claims the page, and the page learns of its new
// controller a moment later), and when this page is not controlled but
// another reader tab is. Missing in an insecure context.
async function workers() {
    const container = navigator.serviceWorker;
    if (!container) return [];
    const registration = await container.getRegistration().catch(() => undefined);
    return [...new Set([container.controller, registration?.active].filter(Boolean))];
}

// Posts the mark for `scope` to `worker` and resolves on its reply, or after
// MARK_WAIT_MS without one (a worker made redundant may never answer).
function post(worker, scope) {
    return new Promise(resolve => {
        const channel = new MessageChannel();
        const done = () => {
            clearTimeout(timer);
            channel.port1.close();
            resolve();
        };
        const timer = setTimeout(done, MARK_WAIT_MS);
        channel.port1.onmessage = done;
        try {
            worker.postMessage({ type: 'invalidate', scope }, [channel.port2]);
        } catch {
            done();
        }
    });
}

// Tells the workers that `scope` ('*', an encoded catalog, or a document's
// API path, as sw.js matches them) is invalid from now on, and waits for
// them to confirm, so the mark is in place before the deletion it announces
// (see sw.js). With no worker, no answer of a worker's can be on its way,
// so there is nothing to mark. A worker that took over meanwhile is marked
// too: after each round the workers are looked up again, until none is new,
// so a mark the old controller never answered still reaches its successor
// before anything is deleted.
async function mark(scope) {
    const marked = new Set();
    for (;;) {
        const fresh = (await workers()).filter(worker => !marked.has(worker));
        if (fresh.length === 0) return;
        for (const worker of fresh) marked.add(worker);
        await Promise.all(fresh.map(worker => post(worker, scope)));
    }
}

// Deletes every cached catalog read and the record of whose they were.
// Deleting needs no free storage, so this works on a full disk too.
export async function wipe() {
    await mark('*');
    await storage(() => Promise.all([content, meta, owner].map(name => caches.delete(name))));
}

// Deletes the entries of `cacheNames` whose URL path `matches`.
function evict(cacheNames, matches) {
    return storage(() => Promise.all(cacheNames.map(async name => {
        const cache = await caches.open(name);
        const keys = await cache.keys();
        await Promise.all(keys.filter(key => matches(new URL(key.url).pathname)).map(key => cache.delete(key)));
    })));
}

// Deletes everything cached for `catalog`, and keeps answers already on
// their way from putting it back.
export async function evictCatalog(catalog) {
    await mark(enc(catalog));
    const prefix = `/api/v1/catalogs/${enc(catalog)}/`;
    await evict([content, meta], path => path.startsWith(prefix));
}

// Deletes the cached copies (short and full) of one document, and keeps
// answers already on their way from putting it back: `kind` is 'concepts'
// or 'documents', as in its API URL.
export async function evictDocument(catalog, kind, path) {
    const exact = `/api/v1/catalogs/${enc(catalog)}/${kind}/${encPath(path)}`;
    await mark(exact);
    await evict([content], url => url === exact);
}

// The concepts and documents of `catalog` readable offline, newest first,
// one per path: [{ path, title, cachedAt }]. Only the recorded owner's
// copies count, as in the worker (sw.js, `ownedCopy`): with no owner
// record, nothing is readable offline.
export function offlineDocuments(catalog) {
    return storage(async () => {
        const record = await (await caches.open(owner)).match(ownerKey);
        const tag = record ? (await record.json().catch(() => null))?.tag : null;
        if (typeof tag !== 'string') return [];
        const cache = await caches.open(content);
        const prefix = `/api/v1/catalogs/${enc(catalog)}/`;
        // Newest last in the cache, so reverse and keep the first of each path.
        const keys = (await cache.keys()).filter(key => new URL(key.url).pathname.startsWith(prefix)).reverse();
        const copies = await Promise.all(keys.map(async key => {
            const response = await cache.match(key);
            if (response?.headers.get('X-OKF-Account') !== tag) return null;
            const body = await response.json().catch(() => null);
            return body?.path ? { path: body.path, title: body.title ?? null, cachedAt: response.headers.get('X-Read-Cached-At') } : null;
        }));
        const seen = new Map();
        for (const copy of copies) {
            if (copy && !seen.has(copy.path)) seen.set(copy.path, copy);
        }
        return [...seen.values()];
    }, []);
}
