// Tests of the reader's display text helpers (src/http/read/js/text.js),
// run with `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { fileName, folderTrail, docLabel, pages, readingMinutes, shortAgo, clock } = await page('text.js');

test('a path names its file and its folders', () => {
    assert.equal(fileName('/a/b/c.md'), 'c.md');
    assert.equal(fileName('c.md'), 'c.md');
    assert.equal(fileName(''), '');
    assert.equal(folderTrail('/infra/network/dns.md'), 'infra › network');
    assert.equal(folderTrail('/dns.md'), '');
    assert.equal(folderTrail(null), '');
});

test('a document is called by its title, else its file name', () => {
    assert.equal(docLabel({ path: '/a/b.md', title: 'Bee' }), 'Bee');
    assert.equal(docLabel({ path: '/a/b.md', title: null }), 'b.md');
    assert.equal(docLabel({ path: '/a/b.md', title: '' }), 'b.md');
});

test('page counts are grouped and singular for one', () => {
    assert.equal(pages(1284), '1,284 pages');
    assert.equal(pages(1), '1 page');
    assert.equal(pages(0), '0 pages');
});

test('reading time is at least a minute', () => {
    assert.equal(readingMinutes(0), 1);
    assert.equal(readingMinutes(1200), 1);
    assert.equal(readingMinutes(9000), 8);
    assert.equal(readingMinutes(undefined), 1);
});

test('a short age counts seconds, minutes, hours and days', () => {
    const now = Date.parse('2026-11-21T12:00:00Z');
    assert.equal(shortAgo('2026-11-21T11:59:33Z', now), '27 s');
    assert.equal(shortAgo('2026-11-21T11:55:00Z', now), '5 min');
    assert.equal(shortAgo('2026-11-21T09:00:00Z', now), '3 h');
    assert.equal(shortAgo('2026-11-19T12:00:00Z', now), '2 d');
    // A time a little ahead of the device's clock reads as now.
    assert.equal(shortAgo('2026-11-21T12:00:05Z', now), '0 s');
});

test('a clock time is hours and minutes on the 24-hour clock', () => {
    assert.equal(clock(new Date(2026, 10, 20, 16, 20).toISOString()), '16:20');
    assert.equal(clock(new Date(2026, 10, 20, 9, 2).toISOString()), '09:02');
});
