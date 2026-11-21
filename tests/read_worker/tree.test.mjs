// Tests of the folder screen (src/http/read/js/screens/tree.js) run in Node
// (`just test-worker`): esm-hooks.mjs maps its imports to tree-doubles/, a
// fake DOM and reads and evictions the test settles by hand, so a deleted or
// unavailable index, another refusal and a navigation during the eviction
// can each be driven exactly.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { KIND, ReadError } = await page('api.js');
const { render } = await page('screens/tree.js', '?doubles');

/** A promise with its `resolve` and `reject`, settled by the test. */
function deferred() {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

/** Lets every queued promise reaction run. */
const ticks = () => new Promise(resolve => setTimeout(resolve, 0));

const TREE = {
    documents: [
        { path: '/index.md', kind: 'index', title: 'Main' },
        { path: '/systems/index.md', kind: 'index', title: 'Systems' },
        { path: '/overview.md', kind: 'concept', title: 'Overview' },
        { path: '/log.md', kind: 'log' },
    ],
};

/** The elements of `nodes` (and theirs) whose id is `id`. */
function byId(nodes, id) {
    const found = [];
    const walk = node => {
        if (!node || typeof node !== 'object') return;
        if (node.attrs?.id === id) found.push(node);
        node.children?.forEach(walk);
    };
    nodes.forEach(walk);
    return found;
}

/** The text of an element's descendants. */
const text = node => (typeof node === 'string' ? node : (node.children ?? []).map(text).join(' '));

/**
 * One render of the root folder: `index` answers the index read (the tree
 * read answers TREE at once), `evict` the eviction. Returns the world: what
 * was appended, evicted and shown as fresh, and the `live` switch.
 */
function scene({ index, evict = async () => {} }) {
    const world = { appended: [], evicted: [], fresh: [], live: true };
    globalThis.treeWorld = {
        read: async path => {
            if (path.endsWith('/tree')) return { data: TREE, offline: false };
            return index(path);
        },
        evictDocument: async (...args) => { world.evicted.push(args); await evict(); },
    };
    world.view = { append: (...nodes) => { world.appended.push(...nodes); } };
    world.ctx = {
        route: { catalog: 'main', folder: '' },
        live: () => world.live,
        freshness: result => { world.fresh.push(result); },
    };
    world.run = () => render(world.view, world.ctx);
    return world;
}

test('a deleted index is evicted and the folder still lists its concepts and folders', async () => {
    const world = scene({ index: async () => { throw new ReadError(KIND.CONCEPT_GONE, 'gone'); } });
    await world.run();
    assert.deepEqual(world.evicted, [['main', 'documents', '/index.md']]);
    assert.equal(byId(world.appended, 'body').length, 0, 'no body for a deleted index');
    assert.match(text(byId(world.appended, 'folders')[0]), /systems/);
    assert.match(text(byId(world.appended, 'concepts')[0]), /Overview/);
    assert.doesNotMatch(text(byId(world.appended, 'concepts')[0]), /log/i);
});

test('an index that is unavailable offline is skipped without evicting anything', async () => {
    const world = scene({ index: async () => { throw new ReadError(KIND.UNAVAILABLE, 'offline'); } });
    await world.run();
    assert.deepEqual(world.evicted, []);
    assert.equal(byId(world.appended, 'body').length, 0);
    assert.equal(byId(world.appended, 'folders').length, 1);
    assert.equal(byId(world.appended, 'concepts').length, 1);
});

test('any other refusal of the index read is thrown to the screen handler', async () => {
    for (const kind of [KIND.FAILED, KIND.LOCKED, KIND.CATALOG_GONE, KIND.SIGNED_OUT]) {
        const error = new ReadError(kind, 'refused');
        const world = scene({ index: async () => { throw error; } });
        await assert.rejects(world.run(), caught => caught === error, kind);
        assert.deepEqual(world.evicted, [], kind);
    }
});

test('a readable index draws its body before the lists', async () => {
    const world = scene({ index: async () => ({ data: { body_html: '<p>Main</p>' }, offline: false }) });
    await world.run();
    assert.equal(byId(world.appended, 'body')[0].html, '<p>Main</p>');
    assert.deepEqual(world.evicted, []);
    assert.equal(world.fresh.length, 2, 'the tree and the index are both reported');
});

test('a folder left during the eviction draws nothing more and reports no freshness', async () => {
    const eviction = deferred();
    const world = scene({
        index: async () => { throw new ReadError(KIND.CONCEPT_GONE, 'gone'); },
        evict: () => eviction.promise,
    });
    const rendering = world.run();
    await ticks();
    assert.equal(world.evicted.length, 1, 'the eviction has started');
    const drawn = world.appended.length;
    const reported = world.fresh.length;
    // Navigation: the screen no longer shows, then the eviction finishes.
    world.live = false;
    eviction.resolve();
    await rendering;
    assert.equal(world.appended.length, drawn, 'no list is appended to the old screen');
    assert.equal(world.fresh.length, reported, 'no freshness after leaving');
});
