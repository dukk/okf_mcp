// Tests of the "Linked from" rows (src/http/read/js/backlinks.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { dedupeBacklinks, backlinkNote } = await page('backlinks.js');

test('a document that links several times is one row with its count, in first-seen order', () => {
    const rows = dedupeBacklinks([
        { path: '/b.md', line: 3, title: 'B' },
        { path: '/a.md', line: 1, title: 'A' },
        { path: '/b.md', line: 7, title: 'B' },
        { path: '/b.md', line: 9, title: 'B' },
    ]);
    assert.deepEqual(rows, [{ path: '/b.md', title: 'B', count: 3 }, { path: '/a.md', title: 'A', count: 1 }]);
    assert.deepEqual(dedupeBacklinks(undefined), []);
});

test('the note counts links, and a log counts entries', () => {
    assert.equal(backlinkNote({ path: '/b.md', count: 3 }), '3 links');
    assert.equal(backlinkNote({ path: '/b.md', count: 1 }), '');
    assert.equal(backlinkNote({ path: '/infra/log.md', count: 6 }), '6 entries');
    assert.equal(backlinkNote({ path: '/log.md', count: 1 }), '1 entry');
});
