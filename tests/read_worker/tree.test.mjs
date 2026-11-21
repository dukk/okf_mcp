// Tests of the folder screen (src/http/read/js/screens/tree.js) run in Node
// (`tools/agentic/gate.sh worker`): esm-hooks.mjs maps its imports to
// tree-doubles/, a fake DOM and a tree read the test answers, so what the
// screen draws from the tree, a refusal, and an answer that arrives after
// the user has left can each be checked. The screen reads only the tree:
// the folder's index opens behind "About", as a document.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { KIND, ReadError } = await page('api.js');
const { render } = await page('screens/tree.js', '?doubles');

/** A promise with its `resolve`, settled by the test. */
function deferred() {
    let resolve;
    const promise = new Promise(yes => { resolve = yes; });
    return { promise, resolve };
}

const ticks = () => new Promise(resolve => setTimeout(resolve, 0));

const TREE = {
    documents: [
        { path: '/index.md', kind: 'index', title: 'Main', description: 'Everything we keep.' },
        { path: '/log.md', kind: 'log', title: null, description: null },
        { path: '/overview.md', kind: 'concept', title: 'Overview', description: 'Start here.' },
        { path: '/systems/index.md', kind: 'index', title: 'Systems', description: 'What runs where.' },
        { path: '/systems/alpha.md', kind: 'concept', title: 'Alpha', description: null },
        { path: '/systems/deep/beta.md', kind: 'concept', title: 'Beta', description: null },
    ],
};

/** The elements of `nodes` (and theirs) matching `test`. */
function find(nodes, test) {
    const found = [];
    const walk = node => {
        if (!node || typeof node !== 'object') return;
        if (test(node)) found.push(node);
        node.children?.forEach(walk);
    };
    nodes.forEach(walk);
    return found;
}

const byId = (nodes, id) => find(nodes, node => node.attrs?.id === id);
const text = node => (typeof node === 'string' ? node : (node.children ?? []).map(text).join(' '));

/**
 * One render of `folder`: `answer` gives the tree read's answer (TREE at
 * once by default). Returns the world: the reads, what was appended, the
 * freshness reports, the titles remembered, and the `live` switch.
 */
function scene({ folder = '', answer = async () => ({ data: TREE, offline: false }) } = {}) {
    const world = { reads: [], appended: [], fresh: [], titles: [], live: true };
    globalThis.treeWorld = {
        read: async path => { world.reads.push(path); return answer(path); },
    };
    world.view = { append: (...nodes) => { world.appended.push(...nodes); } };
    world.ctx = {
        route: { catalog: 'main', folder },
        live: () => world.live,
        freshness: result => { world.fresh.push(result); },
        label: () => 'Main catalog',
        rememberTitles: titles => { world.titles.push(titles); },
    };
    world.run = () => render(world.view, world.ctx);
    return world;
}

test('the root reads only the tree and draws About, its lede, folders, concepts and its log', async () => {
    const world = scene();
    await world.run();
    assert.deepEqual(world.reads, ['/catalogs/main/tree']);
    assert.equal(text(byId(world.appended, 'folder-title')[0]), 'Main');
    assert.equal(find(world.appended, node => node.attrs?.class === 'lede').map(text).join(), 'Everything we keep.');
    assert.equal(byId(world.appended, 'about')[0].attrs.href, '#/d/main/index.md');
    const folders = text(byId(world.appended, 'folders')[0]);
    assert.match(folders, /Systems/);
    assert.match(folders, /What runs where\./);
    assert.match(folders, /\b2\b/, 'alpha and deep/beta');
    const concepts = byId(world.appended, 'concepts')[0];
    assert.match(text(concepts), /Overview/);
    assert.match(text(concepts), /Start here\./);
    assert.doesNotMatch(text(concepts), /log/i);
    assert.match(text(find(world.appended, node => node.attrs?.class === 'sec' && text(node).startsWith('Pages'))[0]), /Pages · 1/);
    assert.equal(byId(world.appended, 'folder-log')[0].attrs.href, '#/d/main/log.md');
    assert.equal(world.fresh.length, 1);
    assert.equal(world.titles[0].get('systems'), 'Systems');
    const [crumbs] = find(world.appended, node => node.tag === 'nav');
    assert.equal(crumbs.root, 'Main catalog');
});

test('a folder without an index has no About row and is titled by its name', async () => {
    const world = scene({ folder: 'systems/deep' });
    await world.run();
    assert.equal(byId(world.appended, 'about').length, 0);
    assert.equal(text(byId(world.appended, 'folder-title')[0]), 'deep');
    assert.equal(find(world.appended, node => node.attrs?.class === 'lede').length, 0);
    assert.match(text(byId(world.appended, 'concepts')[0]), /Beta/);
    assert.equal(byId(world.appended, 'folders').length, 0);
    assert.equal(byId(world.appended, 'folder-log').length, 0);
});

test('a refused tree read is thrown to the screen handler and draws nothing', async () => {
    for (const kind of [KIND.FAILED, KIND.LOCKED, KIND.CATALOG_GONE, KIND.SIGNED_OUT, KIND.UNAVAILABLE]) {
        const error = new ReadError(kind, 'refused');
        const world = scene({ answer: async () => { throw error; } });
        await assert.rejects(world.run(), caught => caught === error, kind);
        assert.deepEqual(world.appended, [], kind);
    }
});

test('a tree that answers after the user has left draws nothing and reports nothing', async () => {
    const answer = deferred();
    const world = scene({ answer: () => answer.promise });
    const rendering = world.run();
    await ticks();
    world.live = false;
    answer.resolve({ data: TREE, offline: false });
    await rendering;
    assert.deepEqual(world.appended, []);
    assert.deepEqual(world.fresh, []);
    assert.deepEqual(world.titles, []);
});
