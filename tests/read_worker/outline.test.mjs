// Tests of "On this page" (src/http/read/js/outline.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { sections, titleHeading } = await page('outline.js');

const heading = (text, level, slug = text.toLowerCase()) => ({ heading: text, slug, level, line: 1, lines: 1, chars: 1 });

test('a title heading and its sub-headings: only the title is dropped', () => {
    const outline = [heading('Beta', 1), heading('Settings', 2), heading('Deep', 3)];
    assert.equal(titleHeading(outline, 'Beta').slug, 'beta');
    assert.deepEqual(sections(outline, 'Beta').map(entry => [entry.heading, entry.depth]), [['Settings', 0], ['Deep', 1]]);
});

test('a concept made of level-1 sections lists every one of them', () => {
    const outline = [heading('One', 1), heading('Two', 1)];
    assert.equal(titleHeading(outline, 'Parts'), null);
    assert.deepEqual(sections(outline, 'Parts').map(entry => [entry.heading, entry.depth]), [['One', 0], ['Two', 0]]);
});

test('the title heading is recognised whatever its case and surrounding spaces', () => {
    assert.equal(sections([heading('alpha ', 1), heading('One', 1)], 'Alpha').length, 1);
    assert.equal(sections([heading('Kubernetes  cluster', 1), heading('Nodes', 2)], ' kubernetes cluster').length, 1);
});

// concept.title is the frontmatter's (src/okf/snapshot.rs `title`), never the
// first heading: without one, nothing proves a heading is the title.
test('an untitled concept keeps every heading, its first level-1 one included', () => {
    const outline = [heading('One', 1), heading('Two', 1)];
    for (const title of [null, undefined, '']) {
        assert.equal(titleHeading(outline, title), null, String(title));
        assert.deepEqual(sections(outline, title).map(entry => entry.heading), ['One', 'Two'], String(title));
    }
});

test('a first heading below level 1 is never the title', () => {
    const outline = [heading('Intro', 2), heading('More', 2)];
    assert.equal(titleHeading(outline, 'Intro'), null);
    assert.equal(sections(outline, 'Intro').length, 2);
    assert.deepEqual(sections([], 'X'), []);
    assert.deepEqual(sections(undefined, 'X'), []);
});
