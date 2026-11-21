// Tests of the reader's refusal handling (src/http/read/js/refusals.js),
// run with `node --test tests/read_worker/` (`just test-worker`). Reads are
// deferred promises the test settles by hand, so a refusal can arrive after
// the user has moved on, or while a sibling read is still out.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { KIND, ReadError } = await page('api.js');
const { refusals, SEVERITY, worst } = await page('refusals.js');

/** A promise with its `resolve` and `reject`, settled by the test. */
function deferred() {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

/** Lets every queued promise reaction run. */
const ticks = () => new Promise(resolve => setTimeout(resolve, 0));

/**
 * One shown screen at `route`: its handler, its `live` switch, the
 * remembered catalog, and a log of every action the handler took.
 */
function screen(route, remembered = route.catalog ?? null) {
    const log = [];
    const world = { live: true, catalog: remembered, log };
    world.handler = refusals(route, () => world.live, {
        signIn: async () => { log.push(['signIn']); },
        evictCatalog: async catalog => { log.push(['evictCatalog', catalog]); },
        evictDocument: async (catalog, kind, path) => { log.push(['evictDocument', catalog, kind, path]); },
        current: () => world.catalog,
        choose: catalog => { log.push(['choose', catalog]); world.catalog = catalog; },
        go: hash => { log.push(['go', hash]); },
        paint: error => { log.push(['paint', error.kind]); },
    });
    // Navigating away: the screen no longer shows, and `catalog` is chosen.
    world.leaveFor = catalog => { world.live = false; world.catalog = catalog; };
    return world;
}

/** A screen's render as show() runs it: a refusal goes to `caught`. */
const rendered = (world, render) => render().catch(error => world.handler.caught(error));

const refusal = (kind, message = kind, body = null) => new ReadError(kind, message, body);

test('a late refusal of catalog A after moving to B evicts A and leaves B alone', async () => {
    const a = screen({ screen: 'tree', catalog: 'A', folder: '' });
    const read = deferred();
    const done = rendered(a, () => read.promise);
    a.leaveFor('B');
    read.reject(refusal(KIND.CATALOG_GONE));
    await done;
    // Evicted, but B is neither forgotten nor navigated away from.
    assert.deepEqual(a.log, [['evictCatalog', 'A']]);
    assert.equal(a.catalog, 'B');
});

test('a refusal of the shown catalog forgets it and opens the picker', async () => {
    const a = screen({ screen: 'tree', catalog: 'A', folder: '' });
    await rendered(a, async () => { throw refusal(KIND.CATALOG_GONE); });
    assert.deepEqual(a.log, [['evictCatalog', 'A'], ['choose', null], ['go', '#/c?gone=A']]);
});

test('a 401 signs in (and so wipes) even after the screen was left, and draws nothing', async () => {
    const a = screen({ screen: 'concept', catalog: 'A', path: 'x.md', section: null });
    const read = deferred();
    const done = rendered(a, () => read.promise);
    a.leaveFor('B');
    read.reject(refusal(KIND.SIGNED_OUT));
    await done;
    assert.deepEqual(a.log, [['signIn']]);
});

test('a missing concept or document is evicted by its own kind; a left screen is not drawn on', async () => {
    const concept = screen({ screen: 'concept', catalog: 'A', path: 'sys/x.md', section: null });
    await rendered(concept, async () => { throw refusal(KIND.CONCEPT_GONE); });
    assert.deepEqual(concept.log, [['evictDocument', 'A', 'concepts', 'sys/x.md'], ['paint', KIND.CONCEPT_GONE]]);

    const documentView = screen({ screen: 'document', catalog: 'A', path: 'log.md' });
    const read = deferred();
    const done = rendered(documentView, () => read.promise);
    documentView.leaveFor('A');
    read.reject(refusal(KIND.CONCEPT_GONE));
    await done;
    assert.deepEqual(documentView.log, [['evictDocument', 'A', 'documents', 'log.md']]);
});

test('settle runs a refusal\'s effects before an unresolved sibling settles', async () => {
    const a = screen({ screen: 'concept', catalog: 'A', path: 'x.md', section: null });
    const { settle, drawWorst } = a.handler;
    const concept = deferred();
    const links = deferred();
    const done = rendered(a, async () => {
        const answers = await Promise.allSettled([settle(concept.promise), settle(links.promise)]);
        const errors = answers.filter(answer => answer.status === 'rejected').map(answer => answer.reason);
        if (errors.length) drawWorst(errors);
    });
    links.reject(refusal(KIND.CATALOG_GONE));
    await ticks();
    // The concept read is still out, and the lost catalog is already evicted.
    assert.deepEqual(a.log, [['evictCatalog', 'A'], ['choose', null]]);
    concept.reject(refusal(KIND.LOCKED, 'Locked.'));
    await done;
    // Each refusal's effects ran once, and only the worst of the two drew.
    assert.deepEqual(a.log, [['evictCatalog', 'A'], ['choose', null], ['go', '#/c?gone=A']]);
});

test('drawWorst draws only the most serious of several refusals', async () => {
    const a = screen({ screen: 'concept', catalog: 'A', path: 'x.md', section: null });
    a.handler.drawWorst([refusal(KIND.UNAVAILABLE), refusal(KIND.CATALOG_GONE), refusal(KIND.LOCKED)]);
    assert.deepEqual(a.log, [['go', '#/c?gone=A']]);

    const b = screen({ screen: 'concept', catalog: 'A', path: 'x.md', section: null });
    b.handler.drawWorst([refusal(KIND.UNAVAILABLE), refusal(KIND.FAILED)]);
    assert.deepEqual(b.log, [['paint', KIND.FAILED]]);
});

test('worst follows SEVERITY whatever the order, and ranks unknown kinds last', () => {
    const kinds = [...SEVERITY].reverse();
    for (let first = 0; first < kinds.length; first += 1) {
        const rotated = [...kinds.slice(first), ...kinds.slice(0, first)];
        assert.equal(worst(rotated.map(kind => refusal(kind))).kind, KIND.SIGNED_OUT);
    }
    assert.equal(worst([new Error('not a refusal'), refusal(KIND.UNAVAILABLE)]).kind, KIND.UNAVAILABLE);
    assert.deepEqual(SEVERITY, [KIND.SIGNED_OUT, KIND.CATALOG_GONE, KIND.CONCEPT_GONE, KIND.LOCKED, KIND.FAILED, KIND.UNAVAILABLE]);
});

test('a refusal settle already handled is drawn by caught, not acted on again', async () => {
    const a = screen({ screen: 'tree', catalog: 'A', folder: '' });
    const read = deferred();
    const done = rendered(a, () => a.handler.settle(read.promise));
    read.reject(refusal(KIND.CATALOG_GONE));
    await done;
    // One eviction and one forget (by settle), one navigation (by caught).
    assert.deepEqual(a.log, [['evictCatalog', 'A'], ['choose', null], ['go', '#/c?gone=A']]);
});

test('caught throws on what is not a refusal, and settle passes it through untouched', async () => {
    const a = screen({ screen: 'tree', catalog: 'A', folder: '' });
    const bug = new TypeError('a bug');
    await assert.rejects(a.handler.caught(bug), error => error === bug);
    await assert.rejects(a.handler.settle(Promise.reject(bug)), error => error === bug && !('handled' in error));
    assert.deepEqual(a.log, []);
});

test('fail acts and draws once for a later refusal, such as a button\'s read', async () => {
    const a = screen({ screen: 'concept', catalog: 'A', path: 'x.md', section: null });
    await a.handler.fail(refusal(KIND.LOCKED, 'Unlock first.', { error: 'UnlockRequired' }));
    assert.deepEqual(a.log, [['paint', KIND.LOCKED]]);
    a.leaveFor('A');
    await a.handler.fail(refusal(KIND.FAILED));
    assert.deepEqual(a.log, [['paint', KIND.LOCKED]]);
});
