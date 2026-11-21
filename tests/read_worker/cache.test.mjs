// Tests of the page's side of the reader's caches (src/http/read/js/cache.js),
// run with `node --test tests/read_worker/` (`just test-worker`). The module
// runs as the page would load it, with `self` as the global, names.js run
// first, the in-memory Cache Storage of storage.mjs as `caches` and a fake
// `navigator.serviceWorker` whose workers answer marks on the MessagePort
// they are given (or never, as a worker made redundant does).
import { beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { page } from './page.mjs';
import { ORIGIN, Storage } from './storage.mjs';

const ROOT = new URL('../../', import.meta.url);
globalThis.self = globalThis;
vm.runInThisContext(readFileSync(new URL('src/http/read/js/names.js', ROOT), 'utf8'));
// The module reads `caches` once when it loads, to know whether storage exists.
globalThis.caches = new Storage().api();
console.warn = () => {};
const { evictCatalog, evictDocument, offlineDocuments, wipe } = await page('cache.js');

const OWNER_KEY = self.READ_CACHES.ownerKey;
const concept = (catalog, path, query = '') => `/api/v1/catalogs/${catalog}/concepts/${path}${query}`;

let storage;
// Every storage call and every mark a worker receives, in order.
let log;

/** A worker that logs each mark and, when `replies`, confirms it. `onMark` runs first. */
function worker(name, { replies = true, onMark = () => {} } = {}) {
    return {
        name,
        postMessage(data, ports) {
            log.push(`mark ${name} ${data.type} ${data.scope}`);
            onMark();
            if (replies) ports[0].postMessage('ok');
        },
    };
}

/** Installs `navigator.serviceWorker` with `controller` and the registration's `active` worker. */
function serviceWorker(controller, active = controller) {
    const container = {
        controller,
        registration: active ? { active } : undefined,
        getRegistration: async () => container.registration,
    };
    Object.defineProperty(globalThis, 'navigator', { value: { serviceWorker: container }, configurable: true });
    return container;
}

/** A navigator without service workers (an insecure context). */
function noServiceWorker() {
    Object.defineProperty(globalThis, 'navigator', { value: {}, configurable: true });
}

beforeEach(() => {
    storage = new Storage();
    log = [];
    storage.refuse = (op, name) => {
        if (op === 'delete') log.push(`delete ${name}`);
        return false;
    };
    globalThis.caches = storage.api();
    noServiceWorker();
});

/** Stores a JSON answer for `path` in `cacheName`, as the worker does. */
async function put(cacheName, path, body, headers = {}) {
    const cache = await storage.api().open(cacheName);
    await cache.put(new URL(path, ORIGIN).href, new Response(JSON.stringify(body), { headers }));
}

async function recordOwner(record) {
    await put('read-owner', OWNER_KEY, record);
}

const cacheNames = () => [...storage.caches.keys()].sort();

test('wipe marks every worker with "*" before it deletes the caches and the owner record', async () => {
    serviceWorker(worker('A'));
    await put('read-content', concept('main', 'a.md'), {}, { 'X-OKF-Account': 'A' });
    await put('read-meta', '/api/v1/catalogs', {});
    await recordOwner({ tag: 'A' });
    await put('read-shell-1', '/read/', {});
    log = [];
    await wipe();
    assert.deepEqual(log.slice(0, 1), ['mark A invalidate *']);
    assert.deepEqual(log.slice(1).sort(), ['delete read-content', 'delete read-meta', 'delete read-owner']);
    assert.deepEqual(cacheNames(), ['read-shell-1']);
});

test('without a worker, wipe deletes at once', async () => {
    await put('read-content', concept('main', 'a.md'), {});
    await wipe();
    assert.deepEqual(cacheNames(), []);
    serviceWorker(null);
    await put('read-content', concept('main', 'a.md'), {});
    await wipe();
    assert.deepEqual(cacheNames(), []);
});

test('a registration\'s active worker newer than the controller is marked in the same round', async () => {
    serviceWorker(worker('A'), worker('B'));
    const started = Date.now();
    await wipe();
    assert.deepEqual(log.slice(0, 2).sort(), ['mark A invalidate *', 'mark B invalidate *']);
    assert.ok(log.slice(2).every(entry => entry.startsWith('delete ')), log.join('\n'));
    assert.ok(Date.now() - started < 500, 'both replied, so nothing waited');
});

test('an uncontrolled page still marks the active worker, which other reader tabs share', async () => {
    serviceWorker(null, worker('B'));
    await wipe();
    assert.equal(log[0], 'mark B invalidate *');
});

test('a mark the replaced controller never answers reaches its successor before anything is deleted', async () => {
    const successor = worker('B');
    let container;
    // A new build takes over as the page posts: the old worker is redundant
    // and drops the message, and the page learns of the new controller.
    const retired = worker('A', {
        replies: false,
        onMark: () => {
            container.controller = successor;
            container.registration = { active: successor };
        },
    });
    container = serviceWorker(retired);
    await put('read-content', concept('main', 'a.md'), {});
    log = [];
    await wipe();
    assert.deepEqual(log.slice(0, 2), ['mark A invalidate *', 'mark B invalidate *']);
    assert.ok(log.slice(2).every(entry => entry.startsWith('delete ')), log.join('\n'));
    assert.deepEqual(cacheNames(), []);
});

test('a worker that never answers delays the deletion by the wait, then it happens anyway', async () => {
    serviceWorker(worker('A', { replies: false }));
    await put('read-content', concept('main', 'a.md'), {});
    log = [];
    const started = Date.now();
    await wipe();
    assert.ok(Date.now() - started >= 900, 'waited for the reply');
    assert.equal(log[0], 'mark A invalidate *');
    assert.equal(log.filter(entry => entry.startsWith('mark')).length, 1, 'marked once');
    assert.deepEqual(cacheNames(), []);
});

test('a worker that refuses the message does not hold up the deletion', async () => {
    serviceWorker({ postMessage: () => { throw new DOMException('gone', 'InvalidStateError'); } });
    await put('read-content', concept('main', 'a.md'), {});
    const started = Date.now();
    await wipe();
    assert.ok(Date.now() - started < 500);
    assert.deepEqual(cacheNames(), []);
});

test('evictCatalog marks the encoded catalog and deletes only its entries in both caches', async () => {
    serviceWorker(worker('A'));
    const catalog = 'team space';
    await put('read-content', concept('team%20space', 'a.md'), {});
    await put('read-content', concept('main', 'a.md'), {});
    await put('read-meta', '/api/v1/catalogs/team%20space/tree', {});
    await put('read-meta', '/api/v1/catalogs/team%20spacex/tree', {});
    await put('read-meta', '/api/v1/catalogs', {});
    log = [];
    await evictCatalog(catalog);
    assert.equal(log[0], 'mark A invalidate team%20space');
    assert.ok(log.indexOf('delete read-content') > 0);
    assert.deepEqual(storage.paths('read-content'), [concept('main', 'a.md')]);
    assert.deepEqual(storage.paths('read-meta').sort(), ['/api/v1/catalogs', '/api/v1/catalogs/team%20spacex/tree']);
});

test('evictDocument marks the document\'s API path and deletes both its forms and nothing else', async () => {
    serviceWorker(worker('A'));
    await put('read-content', concept('main', 'a%20b/c.md'), {});
    await put('read-content', concept('main', 'a%20b/c.md', '?full=true'), {});
    await put('read-content', concept('main', 'a%20b/c.mdx'), {});
    await put('read-content', '/api/v1/catalogs/main/documents/a%20b/c.md', {});
    log = [];
    await evictDocument('main', 'concepts', '/a b/c.md');
    assert.equal(log[0], `mark A invalidate ${concept('main', 'a%20b/c.md')}`);
    assert.deepEqual(storage.paths('read-content').sort(),
        ['/api/v1/catalogs/main/concepts/a%20b/c.mdx', '/api/v1/catalogs/main/documents/a%20b/c.md']);
});

test('the scopes are the strings the worker matches: a read\'s URL path, and its catalog segment', async () => {
    serviceWorker(worker('A'));
    const catalog = 'café (team)';
    const path = '/notes & more/#1?.md';
    // As a screen builds the read (api.js `enc` and `encPath`) and the
    // browser then parses it; the worker marks against that URL's pathname
    // and the CATALOG capture of it.
    const { enc, encPath } = await page('api.js');
    const pathname = new URL(`/api/v1/catalogs/${enc(catalog)}/concepts/${encPath(path)}`, ORIGIN).pathname;
    await evictDocument(catalog, 'concepts', path);
    await evictCatalog(catalog);
    assert.deepEqual(log.filter(entry => entry.startsWith('mark')), [
        `mark A invalidate ${pathname}`,
        `mark A invalidate ${/^\/api\/v1\/catalogs\/([^/]+)\//.exec(pathname)[1]}`,
    ]);
});

test('offlineDocuments lists nothing without an owner record', async () => {
    await put('read-content', concept('main', 'a.md'), { path: 'a.md', title: 'A' }, { 'X-OKF-Account': 'A' });
    assert.deepEqual(await offlineDocuments('main'), []);
    await recordOwner({ tag: null });
    assert.deepEqual(await offlineDocuments('main'), []);
    await recordOwner('not a record');
    assert.deepEqual(await offlineDocuments('main'), []);
    const cache = await storage.api().open('read-owner');
    await cache.put(new URL(OWNER_KEY, ORIGIN).href, new Response('{'));
    assert.deepEqual(await offlineDocuments('main'), []);
});

test('offlineDocuments lists only the owner\'s copies of the catalog, newest first, one per path', async () => {
    await recordOwner({ tag: 'A' });
    const mine = at => ({ 'X-OKF-Account': 'A', 'X-Read-Cached-At': at });
    await put('read-content', concept('main', 'old.md'), { path: 'old.md', title: 'Old' }, mine('t1'));
    await put('read-content', concept('main', 'theirs.md'), { path: 'theirs.md', title: 'Secret' }, { 'X-OKF-Account': 'B' });
    await put('read-content', concept('main', 'bare.md'), { path: 'bare.md', title: 'Bare' });
    await put('read-content', concept('main', 'new.md'), { path: 'new.md' }, mine('t2'));
    await put('read-content', concept('main', 'old.md', '?full=true'), { path: 'old.md', title: 'Old' }, mine('t3'));
    await put('read-content', concept('other', 'x.md'), { path: 'x.md', title: 'X' }, mine('t4'));
    await put('read-content', '/api/v1/catalogs/main/documents/log.md', { path: 'log.md', title: null }, mine('t5'));
    await put('read-content', concept('main', 'broken.md'), 'no path', mine('t6'));
    assert.deepEqual(await offlineDocuments('main'), [
        { path: 'log.md', title: null, cachedAt: 't5' },
        { path: 'old.md', title: 'Old', cachedAt: 't3' },
        { path: 'new.md', title: null, cachedAt: 't2' },
    ]);
});

test('storage that refuses never fails a wipe, an eviction or the offline list', async () => {
    serviceWorker(worker('A'));
    await recordOwner({ tag: 'A' });
    storage.refuse = () => true;
    await wipe();
    await evictCatalog('main');
    await evictDocument('main', 'concepts', 'a.md');
    assert.deepEqual(await offlineDocuments('main'), []);
    assert.deepEqual(log.filter(entry => entry.startsWith('mark')).length, 3, 'the marks still went out');
});

test('without Cache Storage the module loads, does nothing and lists nothing', async () => {
    delete globalThis.caches;
    // A fresh instance of the module, loaded without `caches`.
    const bare = await page('cache.js', '?without-caches');
    serviceWorker(worker('A'));
    await bare.wipe();
    await bare.evictCatalog('main');
    await bare.evictDocument('main', 'concepts', 'a.md');
    assert.deepEqual(await bare.offlineDocuments('main'), []);
});
