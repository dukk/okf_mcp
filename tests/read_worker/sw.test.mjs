// Tests of the reader's service worker (src/http/read/sw.js), run with
// `node --test tests/read_worker/` (`just test-worker`). No dependencies:
// the worker runs in a `node:vm` context whose globals stand in for a
// service worker's, with an in-memory Cache Storage (deleting a cache
// orphans every handle to it, as in a browser), a fetch whose answers the
// test releases one at a time, and a step clock for `performance.now()`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const ROOT = new URL('../../', import.meta.url);
const WORKER = readFileSync(new URL('src/http/read/sw.js', ROOT), 'utf8');
const NAMES = readFileSync(new URL('src/http/read/js/names.js', ROOT), 'utf8');
const ORIGIN = 'https://reader.example.com';
const VERSION = '0123456789abcdef';
const SOURCE = WORKER.replace('__SHELL_VERSION__', VERSION);

const concept = name => `/api/v1/catalogs/main/concepts/${name}.md`;
const WHOLE = JSON.stringify({ path: 'x.md', title: 'X', body_html: '<p>x</p>' });

/** Cache Storage that outlives a worker, so a restart sees what was kept. */
class Storage {
    constructor() {
        this.caches = new Map();
        // `refuse(op, cacheName)` true makes that call reject.
        this.refuse = () => false;
        // Awaited inside every put, so a test can act mid-write.
        this.beforePut = async () => {};
    }

    check(op, name) {
        if (this.refuse(op, name)) throw new DOMException('refused', 'SecurityError');
    }

    /** The CacheStorage object one worker sees. */
    api() {
        const storage = this;
        const tick = () => new Promise(resolve => setImmediate(resolve));
        const urlOf = key => (typeof key === 'string' ? new URL(key, ORIGIN).href : key.url);
        class Cache {
            constructor(name) { this.name = name; this.entries = []; }
            async match(key) {
                storage.check('match', this.name);
                const found = this.entries.find(entry => entry.url === urlOf(key));
                return found ? found.response.clone() : undefined;
            }
            async put(key, response) {
                storage.check('put', this.name);
                const body = await response.arrayBuffer();
                await storage.beforePut(this.name, urlOf(key));
                await tick();
                const url = urlOf(key);
                this.entries = this.entries.filter(entry => entry.url !== url);
                this.entries.push({ url, response: new Response(body, { status: response.status, headers: response.headers }) });
            }
            async delete(key) {
                storage.check('delete', this.name);
                const before = this.entries.length;
                this.entries = this.entries.filter(entry => entry.url !== urlOf(key));
                return before !== this.entries.length;
            }
            async keys() {
                storage.check('keys', this.name);
                return this.entries.map(entry => new Request(entry.url));
            }
        }
        return {
            async open(name) {
                await tick();
                storage.check('open', name);
                if (!storage.caches.has(name)) storage.caches.set(name, new Cache(name));
                return storage.caches.get(name);
            },
            async delete(name) {
                await tick();
                storage.check('delete', name);
                return storage.caches.delete(name);
            },
            async keys() {
                storage.check('keys', null);
                return [...storage.caches.keys()];
            },
            async match(key, options) {
                storage.check('match', options?.cacheName);
                return storage.caches.get(options?.cacheName)?.match(key);
            },
        };
    }

    paths(name) {
        return (this.caches.get(name)?.entries ?? []).map(entry => new URL(entry.url).pathname);
    }

    async ownerTag() {
        const record = await this.caches.get('read-owner')?.match('/read/__owner');
        return record ? (await record.json()).tag : undefined;
    }
}

/** One worker instance on `storage`. Every fetch waits for `answer` unless `route` answers it. */
function startWorker(storage, { route = () => undefined } = {}) {
    const listeners = {};
    const held = [];
    let online = true;
    let clock = 0;
    const context = {
        addEventListener: (type, listener) => { listeners[type] = listener; },
        location: new URL(`${ORIGIN}/read/sw.js`),
        skipWaiting: async () => {},
        clients: { claim: async () => {} },
        caches: storage.api(),
        fetch: async (input, init) => {
            const request = new Request(typeof input === 'string' ? new URL(input, ORIGIN) : input, init);
            if (!online) throw new TypeError('Failed to fetch');
            const routed = route(new URL(request.url).pathname);
            if (routed) return routed;
            return new Promise(resolve => held.push({ path: new URL(request.url).pathname + new URL(request.url).search, resolve }));
        },
        performance: { now: () => (clock += 1) },
        crypto: webcrypto,
        console: { warn: () => {}, log: () => {} },
        // A worker resolves a relative URL against its own location.
        Request: class extends Request {
            constructor(input, init) {
                super(typeof input === 'string' ? new URL(input, ORIGIN) : input, init);
            }
        },
        Response, Headers, URL, DOMException, setTimeout,
    };
    context.self = context;
    vm.createContext(context);
    context.importScripts = () => vm.runInContext(NAMES, context);
    vm.runInContext(SOURCE, context);

    return {
        held,
        goOffline() { online = false; },
        /** Dispatches a GET as a controlled page would; returns respondWith's promise (or undefined). */
        get(path, { mode = 'cors' } = {}) {
            const request = new Request(new URL(path, ORIGIN));
            Object.defineProperty(request, 'mode', { value: mode });
            let answer;
            listeners.fetch({ request, respondWith: promise => { answer = promise; } });
            return answer;
        },
        /** Waits until `count` requests are held. */
        async waitHeld(count) {
            while (held.length < count) await new Promise(resolve => setImmediate(resolve));
        },
        /** Answers the held request for `path` with a 200 for account `tag`. */
        answer(path, tag, body = WHOLE) {
            const at = held.findIndex(entry => entry.path === path);
            assert.ok(at >= 0, `${path} is held`);
            const [entry] = held.splice(at, 1);
            entry.resolve(new Response(body, { status: 200, headers: tag ? { 'X-OKF-Account': tag } : {} }));
        },
        /** Sends an invalidation mark and resolves on the worker's reply. */
        mark(scope) {
            return new Promise(resolve => listeners.message({
                data: { type: 'invalidate', scope },
                ports: [{ postMessage: resolve }],
            }));
        },
        /** Sends any message; resolves to the replies posted on its port. */
        async send(data) {
            const replies = [];
            listeners.message({ data, ports: [{ postMessage: reply => replies.push(reply) }] });
            await new Promise(resolve => setImmediate(resolve));
            return replies;
        },
        install() {
            let done;
            listeners.install({ waitUntil: promise => { done = promise; } });
            return done;
        },
    };
}

/** Requests `paths` together, then answers them all for `tag`. */
async function readAll(worker, paths, tag) {
    const answers = paths.map(path => worker.get(path));
    await worker.waitHeld(paths.length);
    for (const path of paths) worker.answer(path, tag);
    return Promise.all(answers);
}

test('a first visit keeps every answer that arrives together', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [concept('alpha'), concept('beta'), '/api/v1/catalogs'], 'A');
    assert.deepEqual(storage.paths('read-content').sort(), [concept('alpha'), concept('beta')]);
    assert.deepEqual(storage.paths('read-meta'), ['/api/v1/catalogs']);
    assert.equal(await storage.ownerTag(), 'A');
});

test('a takeover wipes the old copies, keeps the new owner\'s and drops a late old answer', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [concept('alpha')], 'A');
    const late = worker.get(concept('late'));
    const first = worker.get(concept('beta'));
    const second = worker.get(concept('gamma'));
    await worker.waitHeld(3);
    worker.answer(concept('beta'), 'B');
    await first;
    worker.answer(concept('late'), 'A');
    worker.answer(concept('gamma'), 'B');
    await Promise.all([late, second]);
    assert.deepEqual(storage.paths('read-content').sort(), [concept('beta'), concept('gamma')]);
    assert.equal(await storage.ownerTag(), 'B');
});

for (const [what, scope] of [
    ['a wipe', '*'],
    ['a lost catalog', 'main'],
    ['a deleted document', concept('slow')],
]) {
    test(`a slow 2xx is not kept after ${what}`, async () => {
        const storage = new Storage();
        const worker = startWorker(storage);
        await readAll(worker, [concept('alpha')], 'A');
        const slow = worker.get(concept('slow'));
        await worker.waitHeld(1);
        await worker.mark(scope);
        worker.answer(concept('slow'), 'A');
        assert.equal((await slow).status, 200, 'the page still gets its answer');
        assert.ok(!storage.paths('read-content').includes(concept('slow')));
    });
}

test('a mark that arrives during the write still drops the copy', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [concept('alpha')], 'A');
    storage.beforePut = async (name, url) => {
        if (url.endsWith('/slow.md')) await worker.mark('main');
    };
    const slow = worker.get(concept('slow'));
    await worker.waitHeld(1);
    worker.answer(concept('slow'), 'A');
    await slow;
    assert.ok(!storage.paths('read-content').includes(concept('slow')));
});

test('the worker answers a mark on its port and ignores other messages', async () => {
    const worker = startWorker(new Storage());
    assert.equal(await worker.mark('*'), 'ok');
    assert.deepEqual(await worker.send({ type: 'other', scope: '*' }), []);
    assert.deepEqual(await worker.send({ type: 'invalidate', scope: 7 }), []);
});

for (const refused of ['open', 'put']) {
    test(`when the owner record cannot be written (${refused}), nothing is kept and a restart cannot serve it`, async () => {
        const storage = new Storage();
        const first = startWorker(storage);
        await readAll(first, [concept('alpha')], 'A');
        storage.refuse = (op, name) => op === refused && name === 'read-owner';
        await readAll(first, [concept('secret')], 'B');
        assert.deepEqual(storage.paths('read-content'), [], 'nothing kept for B');
        assert.notEqual(await storage.ownerTag(), 'A', 'A no longer named as owner');
        // A restarted worker, read by A while offline: B's answer is not there.
        storage.refuse = () => false;
        const restarted = startWorker(storage);
        restarted.goOffline();
        await assert.rejects(restarted.get(concept('secret')));
    });
}

test('after a takeover whose content delete fails, the old owner\'s copy is never served offline', async () => {
    const storage = new Storage();
    const first = startWorker(storage);
    await readAll(first, [concept('private')], 'A');
    storage.refuse = (op, name) => op === 'delete' && name === 'read-content';
    await readAll(first, [concept('other')], 'B');
    first.goOffline();
    await assert.rejects(first.get(concept('private')), 'not served by the worker whose takeover failed');
    const restarted = startWorker(storage);
    restarted.goOffline();
    await assert.rejects(restarted.get(concept('private')), 'not served after a restart');
    storage.refuse = () => false;
    await assert.rejects(restarted.get(concept('private')));
    assert.deepEqual(storage.paths('read-content'), [], 'unowned copies are deleted once storage allows');
});

test('offline, a copy the page evicts while it is being read is not served', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [concept('alpha')], 'A');
    worker.goOffline();
    const content = storage.caches.get('read-content');
    const match = content.match.bind(content);
    content.match = async key => {
        const found = await match(key);
        await worker.mark('main');
        return found;
    };
    await assert.rejects(worker.get(concept('alpha')));
});

test('with the owner record gone but the copies left, a restarted worker serves none of them', async () => {
    const storage = new Storage();
    await readAll(startWorker(storage), [concept('alpha'), '/api/v1/catalogs'], 'A');
    storage.caches.delete('read-owner');
    const restarted = startWorker(storage);
    restarted.goOffline();
    await assert.rejects(restarted.get(concept('alpha')));
    await assert.rejects(restarted.get('/api/v1/catalogs'));
    assert.deepEqual(storage.paths('read-content'), []);
    assert.deepEqual(storage.paths('read-meta'), []);
});

test('an owner\'s own copies are still served offline after a restart', async () => {
    const storage = new Storage();
    await readAll(startWorker(storage), [concept('alpha'), '/api/v1/catalogs'], 'A');
    const restarted = startWorker(storage);
    restarted.goOffline();
    assert.equal((await restarted.get(concept('alpha'))).headers.get('X-Read-Offline'), '1');
    assert.equal((await restarted.get('/api/v1/catalogs')).headers.get('X-Read-Offline'), '1');
    assert.deepEqual(storage.paths('read-content'), [concept('alpha')]);
});

for (const [what, stored, asked] of [
    ['the same form', concept('beta'), concept('beta')],
    ['the other form', `${concept('beta')}?full=true`, concept('beta')],
]) {
    test(`a copy made for another account is not served offline through ${what}, and is deleted`, async () => {
        const storage = new Storage();
        const worker = startWorker(storage);
        await readAll(worker, [concept('alpha')], 'A');
        const content = await storage.api().open('read-content');
        await content.put(stored, new Response(WHOLE, { headers: { 'X-OKF-Account': 'B' } }));
        worker.goOffline();
        await assert.rejects(worker.get(asked));
        assert.deepEqual(storage.paths('read-content'), [concept('alpha')]);
    });
}

test('with storage refused, the network\'s answers still go through and nothing is kept', async () => {
    const storage = new Storage();
    storage.refuse = () => true;
    const worker = startWorker(storage, { route: path => (path === '/read/read.css' ? new Response('css') : undefined) });
    const [answer] = await readAll(worker, [concept('alpha')], 'A');
    assert.equal(answer.status, 200);
    assert.equal(await (await worker.get('/read/read.css')).text(), 'css');
    storage.refuse = () => false;
    assert.deepEqual(storage.paths('read-content'), []);
});

test('an answer without an account tag is not kept', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [concept('alpha')], null);
    assert.deepEqual(storage.paths('read-content'), []);
});

test('offline, a concept is answered by its other form when that form holds the whole body', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    await readAll(worker, [`${concept('full')}?full=true`, concept('whole')], 'A');
    const shortened = worker.get(concept('short'));
    await worker.waitHeld(1);
    worker.answer(concept('short'), 'A', JSON.stringify({ path: 'short.md', title: 'Short' }));
    await shortened;
    worker.goOffline();

    const plain = await worker.get(concept('full'));
    assert.equal(plain.headers.get('X-Read-Offline'), '1');
    assert.ok(plain.headers.get('X-Read-Cached-At'));
    const full = await worker.get(`${concept('whole')}?full=true`);
    assert.equal(full.headers.get('X-Read-Offline'), '1');
    await assert.rejects(worker.get(`${concept('short')}?full=true`), 'a shortened copy is not the whole body');
});

test('the content cache keeps the 200 most recently viewed', async () => {
    const storage = new Storage();
    const worker = startWorker(storage);
    for (let index = 0; index < 201; index += 1) {
        await readAll(worker, [concept(`n${index}`)], 'A');
        if (index === 150) await readAll(worker, [concept('n0')], 'A');
    }
    const paths = storage.paths('read-content');
    assert.equal(paths.length, 200);
    assert.ok(paths.includes(concept('n0')), 're-read entry kept');
    assert.ok(!paths.includes(concept('n1')), 'least recently viewed dropped');
});

test('a navigation under /read/ gets the shell\'s page', async () => {
    const storage = new Storage();
    const shell = await storage.api().open(`read-shell-${VERSION}`);
    await shell.put('/read/', new Response('page'));
    const worker = startWorker(storage);
    const answer = await worker.get('/read/v/main/alpha.md', { mode: 'navigate' });
    assert.equal(await answer.text(), 'page');
    assert.equal(worker.get('/api/v1/session'), undefined, 'other API reads are left alone');
});

/** A server for install: the listing and its files, optionally tampered with. */
function shellServer({ version = VERSION, tamper = false } = {}) {
    const files = { '/read/': 'page', '/read/read.css': 'css' };
    const sha256 = body => createHash('sha256').update(body).digest('hex');
    const listing = {
        version,
        files: Object.entries(files).map(([url, body]) => ({ url, sha256: sha256(body) })),
    };
    return path => {
        if (path === '/read/shell.json') return Response.json(listing);
        if (path in files) return new Response(tamper && path === '/read/read.css' ? 'other build' : files[path]);
        return new Response('missing', { status: 404 });
    };
}

test('install keeps a shell whose version and hashes are this build\'s', async () => {
    const storage = new Storage();
    await startWorker(storage, { route: shellServer() }).install();
    assert.deepEqual(storage.paths(`read-shell-${VERSION}`).sort(), ['/read/', '/read/read.css']);
});

for (const [what, server] of [
    ['a listing from another build', { version: 'ffffffffffffffff' }],
    ['a file that does not hash as listed', { tamper: true }],
]) {
    test(`install refuses ${what} and leaves no shell cache`, async () => {
        const storage = new Storage();
        await assert.rejects(startWorker(storage, { route: shellServer(server) }).install());
        assert.ok(![...storage.caches.keys()].some(name => name.startsWith('read-shell-')));
    });
}
