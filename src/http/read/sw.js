// The reader's service worker (reader design, "Caching"). Its scope is
// /read/, so it never controls a console page. The shell is precached and
// served cache-first; concept and document reads (read-content, capped) and
// listings (read-meta) are network-first with the cache as the offline
// fallback. Only 2xx answers are cached. Every other API read passes
// through uncached, but a 2xx's account tag still decides who owns the
// copies, as a cached answer's does. The worker never makes an error answer
// of its own; the page decides what every refusal means.
importScripts('/read/js/names.js');

const VERSION = '__SHELL_VERSION__';
const SHELL = `${READ_CACHES.shellPrefix}${VERSION}`;
const CONTENT = /^\/api\/v1\/catalogs\/[^/]+\/(concepts|documents)\//;
const META = [
    /^\/api\/v1\/catalogs$/,
    /^\/api\/v1\/catalogs\/[^/]+\/(tree|recent-changes)$/,
    /^\/api\/v1\/catalogs\/[^/]+\/links\//,
];
const CATALOG = /^\/api\/v1\/catalogs\/([^/]+)\//;
const OWNER_KEY = READ_CACHES.ownerKey;

// The worker's own clock for ordering requests against marks and owner
// changes. It is monotonic (a wall-clock change cannot reorder them) and
// lives as long as this worker, as the marks and the in-memory owner do.
const now = () => performance.now();

// Installs only a shell that is entirely this build's: the listing must
// carry this worker's version, and every file must hash as listed. A deploy
// between these requests, or replicas on two builds, fails the install; the
// browser keeps the worker it had and tries again on a later visit.
self.addEventListener('install', event => {
    event.waitUntil((async () => {
        const answer = await fetch('/read/shell.json', { cache: 'no-store' });
        if (!answer.ok) throw new Error(`shell listing: HTTP ${answer.status}`);
        const listing = await answer.json();
        if (listing.version !== VERSION) throw new Error('the shell listing is from another build');
        const responses = await Promise.all(listing.files.map(verified));
        const cache = await caches.open(SHELL);
        try {
            await Promise.all(listing.files.map((file, at) => cache.put(file.url, responses[at])));
        } catch (failure) {
            // A half-written shell is never used; leave no copy of it.
            await caches.delete(SHELL).catch(() => {});
            throw failure;
        }
        await self.skipWaiting();
    })());
});

// `file.url`'s answer, refused unless its bytes hash as `file.sha256`.
async function verified(file) {
    const response = await fetch(file.url, { cache: 'no-store' });
    if (!response.ok) throw new Error(`shell file ${file.url}: HTTP ${response.status}`);
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', await response.clone().arrayBuffer()));
    const hex = [...digest].map(byte => byte.toString(16).padStart(2, '0')).join('');
    if (hex !== file.sha256) throw new Error(`shell file ${file.url} is from another build`);
    return response;
}

self.addEventListener('activate', event => {
    event.waitUntil((async () => {
        const names = await caches.keys().catch(() => []);
        await Promise.all(names
            .filter(name => name.startsWith(READ_CACHES.shellPrefix) && name !== SHELL)
            .map(name => caches.delete(name).catch(() => false)));
        await self.clients.claim();
    })());
});

// Invalidation marks, in memory: the page sends one ('*' for a wipe, an
// encoded catalog for a lost grant, or one document's API path for a
// deleted document) and waits for the reply before it deletes anything.
// The worker then refuses to store an answer to a request that started at
// or before a matching mark, so a slow 2xx cannot put back what a 401 or a
// lost grant removed. Memory needs no storage quota, and a request still on
// its way keeps the worker alive, so the marks last as long as they matter.
// (The Service Worker spec's Try Activate starts a new worker only once this
// one has no events pending, skipWaiting or not, so one worker at a time
// holds the marks that matter; a request the browser times out is the
// exception, and its answer is then no longer this worker's to keep.)
const marks = new Map();

// The account the cached copies belong to, { tag, since }, read from
// read-owner on first use (`undefined` until then). `tag` is null when no
// account owns them; `since` is when the owner took over, on `now()`'s
// clock (-Infinity for a record this worker read: no request of its own
// predates it). The worker, which every reader tab shares, decides
// ownership, so a sign-in as someone else in another tab cannot slip one
// account's answers in among another's.
let owner;
// Bumped whenever the owner changes. A copy is kept only while the epoch it
// was admitted under is current, so an answer admitted for the old owner
// drops its copy when a new one takes over, without touching the new
// owner's answers already on their way.
let epoch = 0;
// Ownership decisions run one at a time, in arrival order.
let ownerQueue = Promise.resolve(null);

// No account owns the copies from now on; answers to requests that started
// before this are never kept.
function disown() {
    owner = { tag: null, since: now() };
    epoch += 1;
}

self.addEventListener('message', event => {
    const scope = event.data?.type === 'invalidate' ? event.data.scope : undefined;
    if (typeof scope !== 'string') return;
    marks.set(scope, now());
    // A wipe also deletes read-owner: the next account to answer owns the
    // copies afresh.
    if (scope === '*') disown();
    event.ports[0]?.postMessage('ok');
});

self.addEventListener('fetch', event => {
    const request = event.request;
    if (request.method !== 'GET') return;
    const url = new URL(request.url);
    if (url.origin !== self.location.origin) return;
    if (request.mode === 'navigate' && url.pathname.startsWith('/read/')) {
        event.respondWith(fromShell(new Request('/read/'), request));
    } else if (CONTENT.test(url.pathname)) {
        event.respondWith(networkFirst(request, READ_CACHES.content, true));
    } else if (META.some(pattern => pattern.test(url.pathname))) {
        event.respondWith(networkFirst(request, READ_CACHES.meta, false));
    } else if (url.pathname.startsWith('/api/')) {
        event.respondWith(passThrough(request));
    } else {
        event.respondWith(fromShell(request, request));
    }
});

// The shell's copy of `key`, else the network's answer to `request`.
// Storage that refuses to read means the network's answer.
async function fromShell(key, request) {
    const cached = await caches.match(key, { cacheName: SHELL }).catch(() => undefined);
    return cached ?? fetch(request);
}

// The network's answer, remembered when it is a 2xx; without a network,
// the remembered copy marked X-Read-Offline. `capped` caches keep only the
// newest READ_CACHES.contentCap entries. Remembering is best effort: a
// failed write (a full disk, QuotaExceededError, storage that refuses to
// open) never costs the page the answer it already has.
async function networkFirst(request, cacheName, capped) {
    const startedAt = now();
    let response;
    try {
        response = await fetch(request);
    } catch (failure) {
        const cached = await ownedCopy(cacheName, request, startedAt);
        if (!cached) throw failure;
        // Re-put so the copy counts as recently viewed; a renewal, never trimmed.
        if (capped) {
            await upkeep(() => store(cacheName, cached.key, cached.response.clone(), { startedAt, held: cached.held, capped: false }));
            // A wipe, an eviction or a takeover during the renewal: the copy
            // is no longer the owner's to read (`store` already dropped it).
            if (!stillOwned(cached, startedAt)) throw failure;
        }
        return stamped(cached.response, 'X-Read-Offline', '1');
    }
    if (response.ok) {
        const copy = stamped(response.clone(), 'X-Read-Cached-At', new Date().toISOString());
        await upkeep(async () => {
            if (invalidatedSince(request.url, startedAt)) return;
            const held = await admit(response.headers.get('X-OKF-Account'), startedAt);
            if (held !== null) await store(cacheName, request, copy, { startedAt, held, capped });
        });
    }
    return response;
}

// The network's answer to any other API read (session, search, whoami),
// unchanged and never stored. A 2xx's account tag still goes through `admit`,
// with its ordering rules: an answer for another account, to a request that
// started after the current owner took over (or after a wipe), deletes the
// old copies and takes over, so a session check alone hands the device to a
// new account. The page gets the answer once that decision is made: when
// boot's session check resolves, the old owner's copies are gone for the
// page's own reads of Cache Storage too (cache.js, `offlineDocuments`). Only
// a takeover waits, once per account change; an answer for the current owner
// or without a tag goes straight back and changes nothing. The worker's own
// offline reads could not overtake it anyway: `ownedCopy` decides in turn
// (`currentOwner`), after every ownership decision queued before it.
async function passThrough(request) {
    const startedAt = now();
    const response = await fetch(request);
    const tag = response.ok ? response.headers.get('X-OKF-Account') : null;
    // `owner` is checked outside the queue: the same tag means `admit` could
    // only keep it, or refuse an answer older than a takeover queued ahead.
    if (tag && owner?.tag !== tag) await admit(tag, startedAt);
    return response;
}

async function loadOwner() {
    try {
        const stored = await (await caches.open(READ_CACHES.owner)).match(OWNER_KEY);
        const record = stored ? await stored.json() : null;
        return { tag: typeof record?.tag === 'string' ? record.tag : null, since: -Infinity };
    } catch {
        return { tag: null, since: -Infinity };
    }
}

// The epoch under which an answer for account `tag`, to a request that
// started at `startedAt`, may be cached, or null when it may not. The first
// answer for a new account, cached or passed through (`passThrough`), makes
// it the owner (`takeOver`). An answer for
// another account to a request from before the current owner took over is
// never kept, so a late answer for the old account cannot take the copies
// back.
function admit(tag, startedAt) {
    return inTurn(async () => {
        if (!tag) return null;
        await knowOwner();
        if (owner.tag === tag) return epoch;
        if (startedAt <= owner.since) return null;
        return takeOver(tag);
    });
}

// Runs `decide` after every ownership decision before it, and resolves to
// its result (null if it throws).
function inTurn(decide) {
    const decision = ownerQueue.then(decide);
    ownerQueue = decision.catch(() => null);
    return ownerQueue;
}

// Reads the owner record on first use.
async function knowOwner() {
    if (owner !== undefined) return;
    const stored = await loadOwner();
    // A wipe may have disowned the copies while the record was read.
    if (owner === undefined) owner = stored;
}

// The current owner, { tag, held } (`held`: the epoch it holds), decided in
// turn with takeovers, so a takeover in progress finishes first. Null when
// no account owns the copies (no record, a wipe, or a failed takeover):
// then whatever read-content and read-meta still hold belongs to nobody,
// and is deleted (best effort).
function currentOwner() {
    return inTurn(async () => {
        await knowOwner();
        if (owner.tag) return { tag: owner.tag, held: epoch };
        await Promise.all([READ_CACHES.content, READ_CACHES.meta]
            .map(name => caches.delete(name).catch(() => false)));
        return null;
    });
}

// The cached answer to `request` in `cacheName` (or its other form, see
// `cachedCopy`), served only when it belongs to the current owner: its own
// X-OKF-Account must be the owner's, and once the copy is read the owner
// must still be the same and the page must not have invalidated it since
// `startedAt`. Returns { key, response, held } or null. A copy made for
// another account is deleted (best effort).
async function ownedCopy(cacheName, request, startedAt) {
    const current = await currentOwner();
    if (!current) return null;
    const cached = await caches.open(cacheName)
        .then(cache => cachedCopy(cache, request))
        .catch(() => null);
    if (!cached) return null;
    if (cached.response.headers.get('X-OKF-Account') !== current.tag) {
        // Only while the owner it was checked against still owns the copies.
        if (epoch === current.held) await upkeep(() => caches.open(cacheName).then(cache => cache.delete(cached.key)));
        return null;
    }
    const owned = { ...cached, tag: current.tag, held: current.held };
    return stillOwned(owned, startedAt) ? owned : null;
}

// Whether `copy` ({ key, tag, held }) still belongs to the owner it was
// checked against, and the page has not invalidated it since `startedAt`.
function stillOwned(copy, startedAt) {
    return epoch === copy.held && owner?.tag === copy.tag
        && !invalidatedSince(copy.key.url ?? copy.key, startedAt);
}

// Makes `tag` the owner. The epoch is bumped first, so the old owner's
// copies still on their way drop themselves. Then the old owner's copies and
// its record go, and only then is the new owner recorded; `tag`'s answers
// are kept only once that record is written. So the durable record never
// names an account other than the one whose copies are cached: at worst
// there is no record, and a restarted worker then treats the next answer as
// a new owner and wipes. Returns the new epoch, or null (keep nothing) when
// the old state could not be deleted, the record could not be written, or a
// wipe came meanwhile.
async function takeOver(tag) {
    owner = { tag, since: now() };
    epoch += 1;
    const mine = epoch;
    try {
        await Promise.all([READ_CACHES.content, READ_CACHES.meta, READ_CACHES.owner]
            .map(name => caches.delete(name)));
        if (epoch !== mine) return null;
        const record = await caches.open(READ_CACHES.owner);
        await record.put(OWNER_KEY, new Response(JSON.stringify({ tag })));
    } catch {
        // Whatever is left belongs to nobody, and this answer is not kept;
        // the next request tries again.
        if (epoch === mine) disown();
        return null;
    }
    return epoch === mine ? mine : null;
}

// Whether the page wiped everything, or evicted `url`'s catalog or the
// document itself (any form of it: the mark is its path, without query),
// at or after `startedAt`.
function invalidatedSince(url, startedAt) {
    const path = new URL(url, self.location.origin).pathname;
    const catalog = CATALOG.exec(path)?.[1];
    return [marks.get('*'), catalog ? marks.get(catalog) : undefined, marks.get(path)]
        .some(at => at !== undefined && at >= startedAt);
}

// Stores `response` under `key` in `cacheName`, unless the page invalidated
// it since `startedAt` or the owner changed since epoch `held`. The cache is
// opened here, not when the request started: a takeover or a wipe deletes
// the cache, and a handle from before would write where nobody reads. The
// checks run again after the write: the page marks before it deletes, and a
// takeover bumps the epoch before it deletes, so an eviction that began
// meanwhile either finds this entry and deletes it, or changed the state
// before this second check, which deletes it.
async function store(cacheName, key, response, { startedAt, held, capped }) {
    const url = key.url ?? key;
    const current = () => epoch === held && !invalidatedSince(url, startedAt);
    if (!current()) return;
    const cache = await caches.open(cacheName);
    await renew(cache, key, response);
    if (!current()) {
        await cache.delete(key);
        return;
    }
    if (capped) await trim(cache);
}

// The cached answer to `request`, or, for a concept, the other form of it
// that holds the same body: a `?full=true` copy answers a plain request, and
// a plain copy answers `?full=true` when it carries the whole body (it was
// not shortened). Returns { key, response } or null.
async function cachedCopy(cache, request) {
    const exact = await cache.match(request);
    if (exact) return { key: request, response: exact };
    const url = new URL(request.url);
    if (!/\/concepts\//.test(url.pathname)) return null;
    if (url.searchParams.get('full') === 'true') {
        url.searchParams.delete('full');
        const plain = await cache.match(url.href);
        const body = plain ? await plain.clone().json().catch(() => null) : null;
        return body?.body_html ? { key: url.href, response: plain } : null;
    }
    url.searchParams.set('full', 'true');
    const full = await cache.match(url.href);
    return full ? { key: url.href, response: full } : null;
}

// Runs one cache write, logging rather than throwing when it fails.
async function upkeep(write) {
    try {
        await write();
    } catch (error) {
        console.warn('reader cache write failed:', error?.name);
    }
}

// Stores `response` for `request` as the cache's newest entry. A cache keeps
// its entries in insertion order, so deleting before putting moves a
// re-read entry to the end, and the front holds the least recently viewed.
async function renew(cache, request, response) {
    await cache.delete(request);
    await cache.put(request, response);
}

// Deletes the oldest entries beyond the cap.
async function trim(cache) {
    const keys = await cache.keys();
    const excess = keys.length - READ_CACHES.contentCap;
    if (excess > 0) await Promise.all(keys.slice(0, excess).map(key => cache.delete(key)));
}

// `response` with one more header (a cached Response's headers are frozen).
function stamped(response, name, value) {
    const headers = new Headers(response.headers);
    headers.set(name, value);
    return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}
