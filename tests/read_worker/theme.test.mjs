// Tests of the reader's theme script (src/http/read/js/theme.js): a classic
// script, run here in a vm context with a fake document and localStorage,
// so a refused storage write can be driven exactly.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SOURCE = readFileSync(new URL('../../src/http/read/js/theme.js', import.meta.url), 'utf8');

/** Runs theme.js against `storage` (a Map, or one whose writes throw). */
function load({ stored = null, denyWrites = false } = {}) {
    const metas = [
        { media: '(prefers-color-scheme: light)', content: '#ffffff' },
        { media: '(prefers-color-scheme: dark)', content: '#191919' },
    ];
    const root = { dataset: {} };
    const items = new Map(stored ? [['okf.read.theme', stored]] : []);
    const localStorage = {
        getItem: key => items.get(key) ?? null,
        setItem: (key, value) => {
            if (denyWrites) throw new DOMException('denied', 'QuotaExceededError');
            items.set(key, value);
        },
        removeItem: key => { items.delete(key); },
    };
    const self = {};
    vm.runInNewContext(SOURCE, {
        self, localStorage,
        document: { documentElement: root, querySelectorAll: () => metas },
    });
    return { theme: self.READ_THEME, root, metas, items };
}

test('a stored choice applies at load and the bars follow it', () => {
    const { theme, root, metas } = load({ stored: 'dark' });
    assert.equal(root.dataset.theme, 'dark');
    assert.deepEqual(metas.map(meta => meta.content), ['#191919', '#191919']);
    assert.equal(theme.current(), 'dark');
});

test('system removes the choice and gives the system bars back', () => {
    const { theme, root, metas, items } = load({ stored: 'dark' });
    theme.choose('system');
    assert.equal(root.dataset.theme, undefined);
    assert.deepEqual(metas.map(meta => meta.content), ['#ffffff', '#191919']);
    assert.equal(items.has('okf.read.theme'), false);
    assert.equal(theme.current(), 'system');
});

test('with storage refusing writes a choice still applies and current() agrees', () => {
    const { theme, root, metas } = load({ denyWrites: true });
    theme.choose('dark');
    assert.equal(root.dataset.theme, 'dark');
    assert.deepEqual(metas.map(meta => meta.content), ['#191919', '#191919']);
    assert.equal(theme.current(), 'dark');
});
