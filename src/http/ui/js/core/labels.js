// Labels for slugs (spec 2026-11-17-okf-display-names-design.md). The
// server computes every label (GET /api/v1/labels); this module only looks
// them up. A slug it does not know (created since the last refresh) is
// shown as itself rather than hidden. Labels are user text: callers put
// them in the page with h() or textContent, never innerHTML.
import { h } from './dom.js';
import { store } from './store.js';

const EMPTY = { users: {}, teams: {}, catalogs: {} };

// Replaces the table of labels; null (a failed read) leaves slugs showing.
export function setLabels(labels) { store.labels = labels ?? EMPTY; }

// An own-property lookup, so a slug such as "constructor" is not read off
// the object prototype.
function lookup(kind, slug) {
    const table = (store.labels ?? EMPTY)[kind] ?? {};
    return Object.hasOwn(table, slug) ? table[slug] : undefined;
}

export const userLabel = slug => lookup('users', slug) ?? slug;
export const teamLabel = slug => lookup('teams', slug) ?? slug;
export const catalogLabel = slug => lookup('catalogs', slug) ?? slug;

// A label with its id beneath, muted: for table cells and drawer titles.
// `label` may already be known (a view's own `label` field).
export function named(label, slug) {
    return h('span', { class: 'named' },
        h('span', { class: 'named-label' }, label),
        h('span', { class: 'id-line muted' }, slug));
}
