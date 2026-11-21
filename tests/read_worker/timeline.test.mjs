// Tests of Home's activity timeline (src/http/read/js/timeline.js), run with
// `tools/agentic/gate.sh worker`. Times are built with local-time Date
// constructors, so day boundaries hold in any time zone.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { BATCH_GAP_MS, FILTERS, GROUP_PREVIEW, changeKind, timeline, dayLabel, dayKey, verb, object, summary } = await page('timeline.js');

const NOW = new Date(2026, 10, 21, 12, 0, 0);
const at = (day, hour, minute, second = 0) => new Date(2026, 10, day, hour, minute, second).toISOString();

/** One change, newest-first order is the caller's. */
function change(path, { actor = 'alex', message = 'import', op = 'import', revision = 1, when = at(21, 11, 0), title = null } = {}) {
    return { path, title, description: null, revision, op, actor, actor_label: null, message, at: when };
}

test('a six-document import is one group of one day', () => {
    const changes = ['/f/a.md', '/f/b.md', '/f/c.md', '/f/d.md', '/f/e.md', '/f/g.md'].map(path => change(path));
    const days = timeline(changes, { now: NOW });
    assert.equal(days.length, 1);
    assert.equal(days[0].label, 'Today');
    assert.equal(days[0].groups.length, 1);
    assert.equal(days[0].groups[0].items.length, 6);
    assert.equal(summary(days[0].groups[0]), 'imported 6 pages');
    assert.equal(GROUP_PREVIEW, 3);
});

test('another actor or another message starts a new group', () => {
    const days = timeline([
        change('/a.md', { actor: 'alex', message: 'one' }),
        change('/b.md', { actor: 'sam', message: 'one' }),
        change('/c.md', { actor: 'sam', message: 'two' }),
        change('/d.md', { actor: 'sam', message: 'two' }),
    ], { now: NOW });
    assert.deepEqual(days[0].groups.map(group => group.items.length), [1, 1, 2]);
    assert.deepEqual(days[0].groups.map(group => group.actor), ['alex', 'sam', 'sam']);
});

test('a gap of two minutes or more starts a new group', () => {
    const close = timeline([
        change('/a.md', { when: at(21, 11, 2, 0) }),
        change('/b.md', { when: at(21, 11, 0, 1) }),
    ], { now: NOW });
    assert.equal(close[0].groups.length, 1, '1 min 59 s apart: one batch');
    const apart = timeline([
        change('/a.md', { when: at(21, 11, 2, 0) }),
        change('/b.md', { when: at(21, 11, 0, 0) }),
    ], { now: NOW });
    assert.equal(apart[0].groups.length, 2, '2 min apart: two batches');
    assert.equal(BATCH_GAP_MS, 120000);
});

test('a day boundary splits a batch, and days are labelled', () => {
    const days = timeline([
        change('/a.md', { when: at(21, 0, 0, 30) }),
        change('/b.md', { when: at(20, 23, 59, 30) }),
        change('/c.md', { when: at(18, 9, 0) }),
    ], { now: NOW });
    assert.equal(days.length, 3, 'a minute apart, but across midnight');
    assert.equal(days[0].label, 'Today');
    assert.equal(days[1].label, 'Yesterday');
    assert.match(days[2].label, /Nov 18/);
    assert.equal(days[0].key, dayKey(at(21, 0, 0, 30)));
    assert.equal(dayLabel(dayKey(NOW), NOW), 'Today');
});

test('each filter keeps its kind of change', () => {
    const changes = [
        change('/new.md', { op: 'create', revision: 1, actor: 'sam' }),
        change('/edited.md', { op: 'update', revision: 3 }),
        change('/imported.md', { op: 'import', revision: 2 }),
        change('/gone.md', { op: 'delete', revision: 4 }),
    ];
    const paths = (filter, me = null) => timeline(changes, { filter, me, now: NOW })
        .flatMap(day => day.groups.flatMap(group => group.items.map(item => item.path)));
    assert.deepEqual(paths('all'), ['/new.md', '/edited.md', '/imported.md', '/gone.md']);
    assert.deepEqual(paths('created'), ['/new.md']);
    assert.deepEqual(paths('edited'), ['/edited.md', '/imported.md']);
    assert.deepEqual(paths('deleted'), ['/gone.md']);
    assert.deepEqual(paths('mine', 'sam'), ['/new.md']);
    assert.deepEqual(paths('mine', null), [], 'signed out of the server: nothing is mine');
    assert.deepEqual(FILTERS.map(filter => filter.id), ['all', 'created', 'edited', 'deleted', 'mine']);
    assert.deepEqual(changes.map(changeKind), ['new', 'edited', 'edited', 'deleted']);
});

test('a group says what was done, to one document or to several', () => {
    const [single] = timeline([change('/k.md', { op: 'update', revision: 2, title: 'Kubernetes cluster' })], { now: NOW })[0].groups;
    assert.equal(verb(single), 'edited');
    assert.equal(object(single), 'Kubernetes cluster');
    assert.equal(summary(single), 'edited Kubernetes cluster');
    const [deleted] = timeline([change('/old.md', { op: 'delete', revision: 2 })], { now: NOW })[0].groups;
    assert.equal(summary(deleted), 'deleted old.md');
    const [mixed] = timeline([
        change('/a.md', { op: 'create', message: 'm' }),
        change('/b.md', { op: 'update', revision: 2, message: 'm' }),
    ], { now: NOW })[0].groups;
    assert.equal(summary(mixed), 'changed 2 pages');
});
