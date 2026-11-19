// The signed-in person's ceiling, a page at a time (issue #736): GET
// /api/v1/me/ceiling?prefix=&after=&limit= answers {ceiling: [{catalog,
// access}], next}. The console never asks for the whole ceiling, which
// grows with the catalogs a person's teams own. Used by the Account page's
// ceiling table and the New key form's catalog picker.
//
// A Find box narrows the list to names starting with what is typed, and
// Load more continues it. As every paged list (lessons.md):
// - the rows drawn, the prefix they answer and the cursor that continues
//   them are one committed state, replaced only when a first page draws
//   and advanced only when a continuation appends;
// - reads are numbered, and an answer a newer read has overtaken is
//   dropped (typing in the Find box races its own earlier reads);
// - a pending read only hides Load more; a failed one keeps the committed
//   rows and cursor (Load more stays to retry) and marks the list
//   possibly out of date.
import { h } from '../core/dom.js';
import { api } from '../core/api.js';

// The page size each read asks for.
export const CEILING_PAGE = 100;

// The query for one page: names starting with `prefix`, after `cursor`
// (null for the first page).
function pageQuery(prefix, cursor) {
    const params = new URLSearchParams({ limit: String(CEILING_PAGE) });
    if (prefix) params.set('prefix', prefix);
    if (cursor !== null) params.set('after', cursor);
    return `/me/ceiling?${params}`;
}

// A pager over the ceiling. `id` prefixes the controls' ids; `draw(rows,
// prefix)` draws the committed rows (called after each committed change).
// Returns { find, footer, start }: the Find box, the footer (the out-of-
// date note and Load more) to place under the rows, and start(), which
// reads the first page for the Find box's prefix again.
export function ceilingPager({ id, draw }) {
    const find = h('input', {
        type: 'search', id: `${id}-find`, autocomplete: 'off',
        placeholder: 'Name starts with', 'aria-label': 'Find catalogs whose name starts with',
    });
    const more = h('button', { type: 'button', id: `${id}-more`, class: 'secondary', hidden: true }, 'Load more');
    const stale = h('p', { id: `${id}-stale`, class: 'muted small', hidden: true },
        'This list may be out of date: a read failed. Try again.');
    // The committed state, replaced or advanced as one.
    let committed = { rows: [], prefix: '', next: null };
    // The newest read's number, and whether it is still unanswered.
    let latest = 0;
    let pending = false;

    function showFooter() {
        more.hidden = pending || committed.next === null;
    }

    // Reads one page: the first for `prefix` (cursor null), or the one
    // after the committed rows. `commit` turns the answer into the new
    // committed state.
    async function read(prefix, cursor, commit) {
        const ticket = ++latest;
        pending = true;
        showFooter();
        let page;
        try {
            page = await api.get(pageQuery(prefix, cursor));
        } catch {
            // Toasted. An overtaken read changes nothing; the newest keeps
            // what is drawn, and its cursor, to retry.
            if (ticket !== latest) return;
            pending = false;
            stale.hidden = false;
            showFooter();
            return;
        }
        if (ticket !== latest) return;
        pending = false;
        stale.hidden = true;
        committed = commit(page);
        draw(committed.rows, committed.prefix);
        showFooter();
    }

    function start() {
        const prefix = find.value.trim().toLowerCase();
        return read(prefix, null, page => ({ rows: page.ceiling, prefix, next: page.next ?? null }));
    }

    more.addEventListener('click', () => {
        // Continues the rows drawn now, with their own prefix.
        const { prefix, next } = committed;
        if (next === null) return;
        read(prefix, next, page => ({
            rows: committed.rows.concat(page.ceiling),
            prefix,
            next: page.next ?? null,
        }));
    });
    find.addEventListener('input', () => { start(); });
    return { find, footer: [stale, more], start };
}
