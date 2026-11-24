// Maintenance runs, newest first; a run's drawer lists its work items, and
// each item links to its concept and its findings.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { catalogLabel } from '../core/labels.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { latest } from '../core/latest.js';
import { maintenancePath, refreshFindingsBadge } from './findings.js';
import { browseHash } from '../core/router.js';

const STATE_KIND = { running: 'info', completed: 'ok', failed: 'error', cancelled: 'muted' };
const OUTCOME_KIND = { completed: 'ok', inconclusive: 'warn', failed: 'error', superseded: 'muted' };
let pageSize = 20;

// A test hook: a journey shrinks the page so a few runs make several pages.
export function setRunsPageSize(size) {
    pageSize = size;
}

const statePill = state => pill(state, STATE_KIND[state] ?? 'muted');

// One page of runs, newest first (`order=desc`); `after` continues past the
// last run of the previous page. The default order, oldest first, is what
// MCP clients get, so the console asks for the newest explicitly.
async function runsPage(catalog, after) {
    const query = new URLSearchParams({ limit: String(pageSize), order: 'desc' });
    if (after) query.set('after', after);
    return api.get(`${maintenancePath(catalog, '/runs')}?${query}`);
}

function columns() {
    return [
        { key: 'state', label: 'State', render: r => statePill(r.state), sort: r => r.state },
        { key: 'id', label: 'Run', class: 'mono small' },
        { key: 'actor', label: 'Actor', class: 'mono' },
        { key: 'work', label: 'Work', render: r => `${r.completed_count}/${r.primary_count} completed`, sort: r => r.primary_count },
        { key: 'other', label: 'Failed / inconclusive / superseded', render: r => `${r.failed_count} / ${r.inconclusive_count} / ${r.superseded_count}` },
        { key: 'created_at', label: 'Created', render: r => ago(r.created_at), sort: r => r.created_at },
        { key: 'ended_at', label: 'Ended', render: r => (r.ended_at ? ago(r.ended_at) : '-'), sort: r => r.ended_at ?? '' },
    ];
}

// Opens a run's drawer, unless the user navigated away from `main` (the
// page's view) while the run loaded.
async function openRun(main, catalog, id) {
    let report;
    try {
        report = await api.get(maintenancePath(catalog, `/runs/${enc(id)}`));
    } catch {
        return;   // the API showed a toast
    }
    if (!main.isConnected) return;
    const { run, work } = report;
    const items = listTable([
        { key: 'pin', label: 'Concept', sort: w => w.pin.path,
            render: w => h('a', { href: browseHash(catalog, w.pin.path), class: 'mono', onclick: closeDrawer }, `${w.pin.path} @ ${w.pin.revision}`) },
        { key: 'reasons', label: 'Lanes', render: w => w.reasons.join(', ') },
        { key: 'outcome', label: 'Outcome', render: w => (w.outcome ? pill(w.outcome, OUTCOME_KIND[w.outcome] ?? 'muted') : pill('leased', 'info')) },
        { key: 'assessment', label: 'Assessment', render: w => (w.assessment ? w.assessment.replaceAll('_', ' ') : '-') },
        { key: 'summary', label: 'Summary', render: w => w.summary ?? '-' },
        { key: 'finding_ids', label: 'Findings', render: w => (w.finding_ids.length
            ? w.finding_ids.flatMap((finding, index) => [index ? ' ' : null,
                h('a', { href: `#/findings/${enc(finding)}`, class: 'mono small', onclick: closeDrawer }, finding)])
            : '0') },
    ], work, { empty: 'No work items.' });
    items.id = 'run-work';
    openDrawer('Run', run.id, h('div', { class: 'run' },
        h('p', { id: 'run-state' }, statePill(run.state), ` by ${run.actor}`),
        h('dl', { class: 'facts' },
            h('dt', {}, 'Created'), h('dd', {}, when(run.created_at)),
            h('dt', {}, 'Deadline'), h('dd', {}, when(run.deadline)),
            h('dt', {}, 'Ended'), h('dd', {}, when(run.ended_at)),
            h('dt', {}, 'Reason'), h('dd', {}, run.reason ?? '-'),
            h('dt', {}, 'Work'), h('dd', {}, `${run.completed_count}/${run.primary_count} completed, ${run.failed_count} failed, `
                + `${run.inconclusive_count} inconclusive, ${run.superseded_count} superseded`)),
        h('h3', {}, 'Work items'),
        items));
}

export async function render(main, rest) {
    const catalog = store.catalog;
    refreshFindingsBadge();
    main.append(h('header', { class: 'page-head' },
        h('div', {}, h('h1', {}, 'Runs'),
            h('p', { class: 'muted' }, catalog ? `Maintenance runs in ${catalogLabel(catalog)}, newest first.` : 'No catalog selected.'))));
    if (!catalog) return;
    const table = listTable(columns(), [], { onRow: run => openRun(main, catalog, run.id), empty: 'No maintenance runs yet.' });
    table.id = 'runs-table';
    // The rows drawn and the cursor that continues them are one committed
    // state: a page replaces both or appends to both, and a failed read
    // changes neither, so "Load older" survives it for a retry.
    let rows = [];
    let next = null;
    let loading = false;
    // `loading` already keeps reads from overlapping (a newer read never
    // starts while one is pending, and Load older appends to the rows drawn,
    // so a later page must not cancel an earlier one), so no read is ever
    // outdated by another. `latest` is used for its other half: a page that
    // answers after the user left draws and commits nothing.
    const reads = latest(main);
    const older = h('button', { type: 'button', id: 'runs-older', class: 'secondary', hidden: true, onclick: () => load() }, 'Load older');
    main.append(table, older);
    // Loads the first page, or the one after the committed cursor. A pending
    // read only hides the button, so one click cannot start two reads.
    async function load() {
        if (loading) return;
        const first = rows.length === 0 && next === null;
        if (!first && !next) return;
        loading = true;
        older.hidden = true;
        const current = reads();
        try {
            const page = await runsPage(catalog, next);
            if (!current()) return;
            rows = rows.concat(page.items);
            next = page.next_after ?? null;
            table.redraw(rows);
        } catch { /* the API showed a toast */ }
        loading = false;
        older.hidden = !next;
    }
    await load();
    if (rest[0]) await openRun(main, catalog, rest[0]);
}
