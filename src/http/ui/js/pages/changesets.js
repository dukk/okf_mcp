// Changesets: the open changesets of the selected catalog (the key's own;
// an admin sees every key's), with owner, idle state and file count. The
// drawer shows the pending diff, computed by the server, and publishes or
// discards. Everything a changeset carries (paths, messages, findings) is
// drawn as text through h(), never as markup.
import { h, fill, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { catalogLabel, named } from '../core/labels.js';
import { toast } from '../core/toast.js';
import { drawerTicket } from '../core/drawer.js';
import { guard, once, recheck } from '../core/pending.js';
import { latest } from '../core/latest.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const COLUMNS = [
    { key: 'changeset', label: 'Handle', class: 'mono',
      render: row => h('span', { class: 'changeset-handle', 'data-test': 'changeset-handle' }, row.changeset) },
    { key: 'catalog', label: 'Catalog', sort: row => row.catalog_label,
      render: row => named(row.catalog_label, row.catalog) },
    { key: 'owner', label: 'Owner', sort: row => row.key_name ?? '',
      render: row => [row.key_name ?? '(deleted key)', ' ', h('span', { class: 'muted small' }, row.actor_label ?? row.actor)] },
    { key: 'created', label: 'Opened', sort: row => row.created, render: row => when(row.created) },
    { key: 'last_change', label: 'Last change', sort: row => row.last_change, render: row => ago(row.last_change) },
    { key: 'files', label: 'Files', sort: row => row.paths.length, render: row => count(row.paths.length, 'file') },
    { key: 'idle', label: 'State', sort: row => (row.idle ? 1 : 0),
      render: row => (row.idle ? pill('idle', 'warn') : pill('active', 'ok')) },
];

export async function render(main) {
    const catalog = store.catalog;
    const scope = store.can('admin') ? "Every key's open changesets" : "Your key's open changesets";
    // The last publish's outcome: it outlives the drawer, which a publish closes.
    const result = h('section', { id: 'changesets-result', class: 'notice', role: 'status', hidden: true });
    const table = listTable(COLUMNS, [], { onRow: row => show(main, row, load, result), empty: 'No open changesets.' });
    table.id = 'changesets-table';
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Changesets'),
                h('p', { class: 'muted' }, `${scope} in ${catalog === null ? 'every catalog' : catalogLabel(catalog)}. Idle ones have had no change within the idle timeout.`)),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'changesets-refresh', class: 'secondary', onclick: () => load() }, 'Refresh'))),
        result,
        table);

    // Numbers the list's reads: a slower older read never draws over a
    // newer one, and none draws once the page was left (core/latest.js).
    const reads = latest(main);
    async function load() {
        const current = reads();
        try {
            const listed = await api.get(`/changesets?catalog=${enc(catalog ?? '')}`);
            if (current()) table.redraw(listed.changesets);
        } catch { /* toasted */ }
    }
    await load();
}

// One changeset's drawer: its diff (loaded on open), Publish and Discard.
// The drawer's ticket (core/drawer.js) is its claim: once the user closes
// it, opens another, or leaves the page, nothing late fills or closes it.
// Publish and Discard are one pending write per handle (core/pending.js),
// so neither can start while the other, or a second copy, runs.
//
// `showing` is the drawer open now: {handle, ticket}. A write that closes a
// changeset closes whichever drawer shows that handle when it settles, even
// one reopened while it ran, and never a drawer showing another.
let showing = null;

function closeShown(handle) {
    if (showing?.handle === handle && showing.ticket.current()) showing.ticket.close();
}

async function show(main, row, reload, result) {
    const ticket = drawerTicket(main);
    showing = { handle: row.changeset, ticket };
    const key = `changeset:${row.changeset}`;
    const writable = store.canOn('write', row.catalog);
    const diff = h('div', { id: 'changeset-diff' }, h('p', { class: 'muted' }, 'Loading the diff...'));
    const publisher = writable ? publishForm(row, key, ticket, reload, result) : null;
    const discardButton = writable
        ? guard(h('button', { type: 'button', id: 'changeset-discard', class: 'danger',
            onclick: () => once(key, () => discardOne(row, reload)) }, 'Discard'), key)
        : null;
    ticket.open(row.changeset,
        `${row.catalog_label} - ${row.key_name ?? 'deleted key'} (${row.actor_label ?? row.actor}) - last change ${ago(row.last_change)}`,
        row.idle ? h('p', {}, pill('idle', 'warn'), ' No change within the idle timeout.') : null,
        diff,
        publisher,
        h('div', { class: 'drawer-actions' }, discardButton));
    let content;
    try {
        const pending = await api.get(`/changesets/${enc(row.changeset)}/diff`);
        content = pending.files.length
            ? pending.files.map(file => h('section', { class: 'diff-file' },
                h('h3', { class: 'mono small' }, `${file.change} ${file.path}`, ' ', file.stale ? pill('stale', 'warn') : null),
                // The diff is a text child (a text node, like textContent), never markup.
                h('pre', { class: 'diff' }, file.diff)))
            : [h('p', { class: 'muted' }, 'No pending files: the changeset is empty.')];
    } catch (error) {
        content = [h('p', { class: 'error' }, `The diff could not be loaded: ${error.message}`)];
    }
    if (!ticket.current()) return;
    diff.replaceChildren(...content);
}

// The Publish form: a revision message (required, not only whitespace) and
// a button that asks for confirmation, then posts it. A refusal keeps the
// message and the drawer, and shows its details (conflicting files, lint
// errors) under the form; nothing is closed or discarded.
function publishForm(row, key, ticket, reload, result) {
    const message = h('textarea', { id: 'changeset-message', name: 'message', rows: '2',
        placeholder: 'What changed, as on every revision this writes', 'aria-label': 'Revision message' });
    const blank = () => message.value.trim() === '';
    const publish = guard(h('button', { type: 'submit', id: 'changeset-publish' }, 'Publish'), key, () => !blank());
    const refusal = h('div', { id: 'changeset-publish-error', class: 'error small', role: 'alert', hidden: true });
    message.addEventListener('input', () => recheck(publish));
    const form = h('form', { id: 'changeset-publish-form' },
        h('label', {}, 'Revision message', message),
        h('div', { class: 'drawer-actions' }, publish),
        refusal);
    form.addEventListener('submit', event => {
        event.preventDefault();
        if (blank()) {
            showRefusal(refusal, 'Enter a message saying what changed.');
            return;
        }
        once(key, async () => {
            const ok = await confirmDialog({
                title: 'Publish this changeset?',
                message: `${count(row.paths.length, 'pending file')} in ${row.catalog_label} will be published with the message "${message.value.trim()}", and the handle ${row.changeset} closed.`,
                action: 'Publish',
                danger: false,
            });
            if (!ok) return;
            refusal.hidden = true;
            let answer;
            try {
                answer = await api.post(`/changesets/${enc(row.changeset)}/publish`, { message: message.value });
            } catch (error) {
                if (ticket.current()) showRefusal(refusal, ...refusalDetails(error));
                return;
            }
            if (!answer.published) {
                // Nothing was published: the changeset holds no entries and stays open.
                if (ticket.current()) showRefusal(refusal, 'Nothing to publish: the changeset has no pending files, so it stays open.');
            } else {
                closeShown(row.changeset);
                showResult(result, answer);
                toast(`Published ${answer.changeset}.`, 'ok');
            }
            // `result` lives on the page: detached, the user has moved on.
            if (result.isConnected) await reload();
        });
    });
    return form;
}

function showRefusal(el, ...children) {
    fill(el, ...children);
    el.hidden = false;
}

// A refusal's message, with the conflicting files or the lint errors it
// names, each line as text.
function refusalDetails(error) {
    const body = error.body ?? {};
    const lines = [];
    if (body.error === 'Conflict') {
        for (const file of body.files ?? []) {
            const base = file.base_revision ?? 'none';
            const now = file.current_revision ?? 'none';
            lines.push(`${file.path}: changed since this changeset read it (read at revision ${base}, now ${now})`);
        }
    } else if (body.error === 'LintFailed') {
        for (const finding of body.errors ?? []) {
            lines.push(`${finding.file}:${finding.line} ${finding.rule}: ${finding.message}`);
        }
    }
    const note = body.error === 'Conflict' || body.error === 'LintFailed'
        ? 'Nothing was published; the changeset is still open. Fix it through the agent or an MCP client, then publish again.'
        : null;
    return [h('p', {}, `Not published: ${error.message}`),
        lines.length ? h('ul', { dataset: { test: 'publish-refusal-details' } }, ...lines.map(line => h('li', { class: 'mono' }, line))) : null,
        note ? h('p', { class: 'muted' }, note) : null];
}

// The page's record of a publish that went through: what was written, and
// the lint advice and replayed paths the server returned.
function showResult(el, answer) {
    const written = answer.revisions.length
        ? `Published ${answer.changeset} to ${answer.catalog_label}: ${count(answer.revisions.length, 'revision')}.`
        : `Published ${answer.changeset} to ${answer.catalog_label}: every edit already matched the published text, so no revision was written and the changeset is closed.`;
    fill(el,
        h('p', { 'data-test': 'publish-summary' }, written),
        answer.revisions.length
            ? h('ul', {}, ...answer.revisions.map(rev => h('li', { class: 'mono small' }, `${rev.op} ${rev.path} (revision ${rev.revision})`)))
            : null,
        answer.warnings.length
            ? [h('p', {}, 'Lint warnings:'),
                h('ul', {}, ...answer.warnings.map(w => h('li', { class: 'mono small' }, `${w.file}:${w.line} ${w.rule}: ${w.message}`)))]
            : null,
        answer.replayed.length
            ? [h('p', {}, 'Bookkeeping replayed onto newer text:'),
                h('ul', {}, ...answer.replayed.map(path => h('li', { class: 'mono small' }, path)))]
            : null);
    el.hidden = false;
}

async function discardOne(row, reload) {
    const ok = await confirmDialog({
        title: 'Discard this changeset?',
        message: `${count(row.paths.length, 'pending file')} in ${row.catalog_label} will be dropped, and the handle ${row.changeset} closed for good. This cannot be undone.`,
        action: 'Discard',
    });
    if (!ok) return;
    try {
        const result = await api.del(`/changesets/${enc(row.changeset)}`);
        closeShown(row.changeset);
        toast(`Discarded ${count(result.removed, 'file')} from ${result.changeset}.`, 'ok');
        await reload();
    } catch { /* toasted */ }
}
