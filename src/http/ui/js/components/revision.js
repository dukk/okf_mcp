// One revision in the drawer: its diff from the revision before, and its
// text. Browse's History tab and the Recent changes page both open it. The
// revision route answers for a deleted document too, so a deletion opens
// like any other revision, without a live document to navigate to.
import { h } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { drawerTicket } from '../core/drawer.js';
import { when } from '../core/format.js';

// A path's segments, each encoded; a leading '/' is dropped.
const encPath = path => String(path).replace(/^\/+/, '').split('/').map(enc).join('/');

// `view` is the page's container. The drawer is outside it, so the claim
// (core/drawer.js) is taken before the read and the drawer opens only while
// the claim is current: the page is still shown (a navigation or a catalog
// switch detaches `view`) and no newer revision, other drawer or close
// came first. A late answer never fills a newer drawer or page.
// `row` is a history entry: {revision, actor, actor_label?, message, at}.
export async function openRevision(view, catalog, path, row) {
    const ticket = drawerTicket(view);
    let revision;
    try {
        revision = await api.get(`/catalogs/${enc(catalog)}/revisions/${row.revision}/${encPath(path)}`);
    } catch {
        return;   // the API showed a toast
    }
    if (!ticket.current()) return;
    const lines = revision.diff.split('\n').map(line => h('span', {
        class: line.startsWith('+') && !line.startsWith('+++') ? 'add'
            : line.startsWith('-') && !line.startsWith('---') ? 'del' : null,
    }, `${line}\n`));
    // The actor's label when the directory knows one, with its id.
    const actor = row.actor_label && row.actor_label !== row.actor ? `${row.actor_label} (${row.actor})` : row.actor;
    ticket.open(`${revision.path} @ ${revision.revision}`, `${revision.op} by ${actor}, ${when(row.at)}: ${row.message}`,
        h('h3', {}, 'Changes from the previous revision'),
        h('pre', { class: 'diff', id: 'revision-diff' }, lines),
        revision.text === null
            ? h('p', { class: 'muted', id: 'revision-no-text' }, 'No text at this revision (deleted, or not text).')
            : h('details', {}, h('summary', {}, 'Text at this revision'), h('pre', { class: 'raw', id: 'revision-text' }, revision.text)));
}
