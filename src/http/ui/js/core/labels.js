// Labels for slugs (spec 2026-11-17-okf-display-names-design.md). The
// server computes every label; this module only looks them up. Two tables:
// GET /api/v1/labels' answer (the caller's own user and teams, and the
// catalogs it reaches with their owner teams), read by the shell, and the
// labels of the other users and teams a view shows, read on demand
// (ensureLabels: GET /api/v1/labels/named, at most LABELS_NAMED of each
// per request), never the whole population (issue #749). A slug neither
// knows (created since, or one the caller may not see) is shown as itself
// rather than hidden. Labels are user text: callers put them in the page
// with h() or textContent, never innerHTML.
import { h } from './dom.js';
import { store } from './store.js';
import { api, enc } from './api.js';

const EMPTY = { users: {}, teams: {}, catalogs: {} };

// The most users, and the most teams, one GET /labels/named may name (the
// server's NAMED_MAX).
const LABELS_NAMED = 100;

// The on-demand table: kind -> slug -> { label, generation }, where `label`
// is null for a slug asked for that the server did not answer (unknown, or
// not the caller's to see). Memory only; emptied on sign-out.
let onDemand = { users: new Map(), teams: new Map() };
// Bumped by each shell refresh: an entry read before it is still shown, but
// read again by the next ensureLabels that wants it (a display name may
// have changed), so a redraw between the two never falls back to a slug.
let generation = 0;

// Replaces the table of labels; null (a failed read) leaves slugs showing.
// The on-demand labels are kept but marked for reading again.
export function setLabels(labels) {
    store.labels = labels ?? EMPTY;
    generation += 1;
}

// Forgets every label, on sign-out: the next credential may see fewer.
export function forgetLabels() {
    store.labels = null;
    onDemand = { users: new Map(), teams: new Map() };
    generation += 1;
}

// An own-property lookup, so a slug such as "constructor" is not read off
// the object prototype.
function lookup(kind, slug) {
    const table = (store.labels ?? EMPTY)[kind] ?? {};
    if (Object.hasOwn(table, slug)) return table[slug];
    return onDemand[kind]?.get(slug)?.label ?? undefined;
}

// Reads the labels of the users and teams a view is about to show that the
// shell's table does not hold and that were not read since the last
// refresh, in requests of at most LABELS_NAMED of each. `wanted` is
// { users, teams }, each an iterable of slugs (null and empty entries
// skipped). Never rejects: a failed read leaves those slugs as they were
// (themselves, or the label read before) and asks again next time, so a
// view's own answer is always drawn.
export async function ensureLabels({ users = [], teams = [] } = {}) {
    const asked = generation;
    const table = onDemand;
    const missing = (kind, slugs) => [...new Set([...slugs].filter(slug => {
        if (!slug) return false;
        const shell = (store.labels ?? EMPTY)[kind] ?? {};
        return !Object.hasOwn(shell, slug) && table[kind].get(slug)?.generation !== asked;
    }))];
    const want = { users: missing('users', users), teams: missing('teams', teams) };
    const reads = [];
    for (let start = 0; start < Math.max(want.users.length, want.teams.length); start += LABELS_NAMED) {
        const part = { users: want.users.slice(start, start + LABELS_NAMED), teams: want.teams.slice(start, start + LABELS_NAMED) };
        const query = Object.entries(part).filter(([, slugs]) => slugs.length)
            .map(([kind, slugs]) => `${kind}=${slugs.map(enc).join(',')}`).join('&');
        reads.push(api.get(`/labels/named?${query}`, { quiet: true }).then(answer => {
            // Signed out meanwhile: the answer belongs to the old credential.
            if (table !== onDemand) return;
            for (const kind of ['users', 'teams']) {
                for (const slug of part[kind]) {
                    // A read asked for after a later refresh may have settled
                    // first: an older answer never replaces its newer label.
                    if ((table[kind].get(slug)?.generation ?? -1) > asked) continue;
                    const label = answer?.[kind]?.[slug];
                    table[kind].set(slug, { label: typeof label === 'string' ? label : null, generation: asked });
                }
            }
        }, () => { /* the slugs keep what they show; asked again next time */ }));
    }
    await Promise.all(reads);
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
