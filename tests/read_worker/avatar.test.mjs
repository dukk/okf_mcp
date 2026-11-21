// Tests of the avatar parts (src/http/read/js/avatar.js), run with
// `tools/agentic/gate.sh worker`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { page } from './page.mjs';

const { AVATAR_COLOURS, initials, avatarClass } = await page('avatar.js');

test('initials take the first letters of the first two words', () => {
    assert.equal(initials('Alex Kim'), 'AK');
    assert.equal(initials('alex'), 'AL');
    assert.equal(initials('claude-code/model-name'), 'CC');
    assert.equal(initials('  sam  '), 'SA');
    assert.equal(initials('émile zola'), 'ÉZ');
    assert.equal(initials(''), '?');
    assert.equal(initials(null), '?');
});

test('the colour is stable for an actor and spread across actors', () => {
    assert.equal(avatarClass('alex'), avatarClass('alex'));
    const classes = new Set();
    for (let n = 0; n < 40; n += 1) {
        const name = avatarClass(`actor-${n}`);
        assert.match(name, /^avatar-\d$/);
        assert.ok(Number(name.slice('avatar-'.length)) < AVATAR_COLOURS, name);
        classes.add(name);
    }
    assert.ok(classes.size >= 4, `only ${classes.size} colours for 40 actors`);
    assert.match(avatarClass(''), /^avatar-\d$/);
});
