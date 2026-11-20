// Tests of the reader's read client (src/http/read/js/api.js), run with
// `node --test tests/read_worker/` (`just test-worker`). `fetch` is replaced
// per test by a fake that answers from a table.
import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { classify, encPath, KIND, read, readAll, ReadError, signOut } = await page('api.js');

const realFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = realFetch; });

/** Replaces fetch: `answer(url, init)` returns a Response or throws; every call is logged. */
function fakeFetch(answer) {
    const calls = [];
    globalThis.fetch = async (url, init) => {
        calls.push({ url, init });
        return answer(url, init);
    };
    return calls;
}

const json = (body, status = 200, headers = {}) =>
    new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } });

test('classify follows every row of the reader design\'s error table', () => {
    // 401: signed out, whatever the body.
    assert.equal(classify(401, { error: 'Unauthenticated' }), KIND.SIGNED_OUT);
    assert.equal(classify(401, null), KIND.SIGNED_OUT);
    // A catalog no longer reachable.
    assert.equal(classify(403, { error: 'Forbidden' }), KIND.CATALOG_GONE);
    assert.equal(classify(404, { error: 'CatalogNotSelected' }), KIND.CATALOG_GONE);
    assert.equal(classify(404, { error: 'UnknownName' }), KIND.CATALOG_GONE);
    // A concept (or a folder's index.md) that no longer exists.
    assert.equal(classify(404, { error: 'NotFound' }), KIND.CONCEPT_GONE);
    // Locked: keep the caches, show the server's message.
    assert.equal(classify(403, { error: 'UnlockRequired' }), KIND.LOCKED);
    assert.equal(classify(403, { error: 'Suspended' }), KIND.LOCKED);
});

test('classify calls anything else a failure, never a revocation', () => {
    assert.equal(classify(403, { error: 'MaintenanceOff' }), KIND.FAILED);
    assert.equal(classify(403, null), KIND.FAILED);
    assert.equal(classify(404, null), KIND.FAILED);
    assert.equal(classify(404, { error: 'UnknownChangeset' }), KIND.FAILED);
    // A kind only counts with its own status.
    assert.equal(classify(404, { error: 'Forbidden' }), KIND.FAILED);
    assert.equal(classify(403, { error: 'NotFound' }), KIND.FAILED);
    for (const status of [400, 409, 410, 422, 500, 503]) assert.equal(classify(status, { error: 'Busy' }), KIND.FAILED);
});

test('read answers the data and whether the worker served it offline', async () => {
    const calls = fakeFetch(() => json({ path: 'a.md' }, 200, { 'X-Read-Offline': '1', 'X-Read-Cached-At': '2026-01-01T00:00:00Z' }));
    assert.deepEqual(await read('/catalogs/main/concepts/a.md'),
        { data: { path: 'a.md' }, offline: true, cachedAt: '2026-01-01T00:00:00Z' });
    assert.equal(calls[0].url, '/api/v1/catalogs/main/concepts/a.md');
    assert.equal(calls[0].init.headers.Accept, 'application/json');
    fakeFetch(() => json({ path: 'a.md' }));
    assert.deepEqual(await read('/x'), { data: { path: 'a.md' }, offline: false, cachedAt: null });
});

test('read turns a refusal into a classified ReadError carrying the body', async () => {
    fakeFetch(() => json({ error: 'Forbidden', message: 'no access' }, 403));
    const refused = await read('/x').catch(error => error);
    assert.ok(refused instanceof ReadError);
    assert.equal(refused.kind, KIND.CATALOG_GONE);
    assert.equal(refused.message, 'no access');
    assert.deepEqual(refused.body, { error: 'Forbidden', message: 'no access' });
    // A body that is not JSON still gives a status message.
    fakeFetch(() => new Response('<html>', { status: 502 }));
    const failed = await read('/x').catch(error => error);
    assert.deepEqual([failed.kind, failed.message, failed.body], [KIND.FAILED, 'HTTP 502', null]);
});

test('read without a network or a cached copy is unavailable', async () => {
    fakeFetch(() => { throw new TypeError('Failed to fetch'); });
    const error = await read('/x').catch(failure => failure);
    assert.deepEqual([error instanceof ReadError, error.kind], [true, KIND.UNAVAILABLE]);
});

test('readAll follows the cursors and keeps the first page\'s offline state', async () => {
    const pages = {
        '/api/v1/catalogs/main/recent-changes?limit=5': { changes: [1, 2], next: 'c 1' },
        '/api/v1/catalogs/main/recent-changes?limit=5&after=c%201': { changes: [3], next: 'c2' },
        '/api/v1/catalogs/main/recent-changes?limit=5&after=c2': { changes: [4], next: null },
    };
    const calls = fakeFetch(url => json(pages[url], 200, { 'X-Read-Offline': '1' }));
    const all = await readAll('/catalogs/main/recent-changes?limit=5', 'changes');
    assert.deepEqual(all, { data: [1, 2, 3, 4], offline: true, cachedAt: null });
    assert.equal(calls.length, 3);
    fakeFetch(() => json({ catalogs: ['a'] }));
    assert.deepEqual((await readAll('/catalogs', 'catalogs')).data, ['a']);
});

test('readAll stops at a cursor that does not move', async () => {
    const calls = fakeFetch(() => json({ items: ['x'], next: 'same' }));
    assert.deepEqual((await readAll('/list', 'items')).data, ['x', 'x']);
    assert.deepEqual(calls.map(call => call.url), ['/api/v1/list', '/api/v1/list?after=same']);
});

test('signOut sends the CSRF token and counts a 401 as signed out', async () => {
    const calls = fakeFetch(() => new Response(null, { status: 204 }));
    assert.equal(await signOut('token'), true);
    assert.deepEqual([calls[0].url, calls[0].init.method, calls[0].init.headers['X-OKF-CSRF']],
        ['/api/v1/session', 'DELETE', 'token']);
    fakeFetch(() => new Response(null, { status: 401 }));
    assert.equal(await signOut('token'), true);
    fakeFetch(() => new Response(null, { status: 500 }));
    assert.equal(await signOut('token'), false);
    fakeFetch(() => { throw new TypeError('Failed to fetch'); });
    assert.equal((await signOut('token').catch(error => error)).kind, KIND.UNAVAILABLE);
});

test('encPath encodes each segment of a catalog path', () => {
    assert.equal(encPath('/a b/café #1?.md'), 'a%20b/caf%C3%A9%20%231%3F.md');
    assert.equal(encPath('a.md'), 'a.md');
});
