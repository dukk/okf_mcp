// The audit log (M14 Task 8c): administrative changes, catalog writes,
// exports and admin reads, newest first. An every-catalog admin reads
// everything and may filter by any team's name; a person signed in with an
// account picks one of the teams they own and reads that team's entries.
// The server decides what each reader sees (an owner gets operator staff as
// "an operator", no staff IPs and no unlock reasons): this page only draws
// the answer, and the export downloads the same filters as JSON Lines.
//
// Every table load (a filter change, the team picker, Refresh) is numbered,
// and only the newest one draws; a "Load more" answer appends only while
// the load it continues is still the newest. A failed load keeps the rows
// drawn, marked as possibly out of date, with its reason on screen and
// every control usable.
import { h, fill } from '../core/dom.js';
import { api } from '../core/api.js';
import { store } from '../core/store.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { canAdministerPeople, freshAccount } from '../core/shell.js';
import { userHash } from '../core/router.js';

// The filters, by query parameter, in the order they are sent. Empty values
// are left out, so the query names only what the reader chose.
const FILTERS = ['team', 'action', 'actor', 'target', 'since', 'until'];
const NO_TEAM = "You own no team; the audit log shows your teams' entries to their owners.";
const DENIED = 'The audit log is for an admin key that reaches every catalog, '
    + 'or for a team owner signed in with their account.';

// Who acted: "an operator" when the server says so (a team owner reading
// staff's entries), else the person and what they acted through (the key's
// or grant's label, or "console session"), or the label alone (the CLI, the
// system); the key or grant id is the tooltip, and the address shows when
// the reader may see it. A person shows by their label (a user's own text,
// drawn as text by `h`, never as HTML); the filters still match slugs.
function actorCell(entry) {
    const actor = entry.actor;
    if (actor.kind === 'operator') return h('span', { class: 'muted' }, 'an operator');
    const who = actor.person_label ?? actor.person ?? actor.label;
    const via = actor.person ? actor.label : null;
    return h('span', { title: actor.credential ?? actor.person ?? null }, who,
        via ? [' ', h('span', { class: 'mono small muted' }, via)] : null,
        entry.ip ? [' ', h('span', { class: 'mono small muted' }, entry.ip)] : null);
}

// What it was about: "user NAME" or "KIND NAME". A target the reader may not
// see (operator staff, for a team owner) has neither a name nor a person.
// For an admin, a user target links to that user's drawer on the Users page,
// whose Recent sign-ins lists what the audit log does not.
function targetCell(entry, admin) {
    const { kind, name, person, person_label: personLabel, target_label: targetLabel } = entry.target;
    if (kind === 'user') {
        if (!person) return 'user an operator';
        const linked = admin && !person.startsWith('deleted user');
        const shown = personLabel ?? person;
        return h('span', {}, 'user ',
            linked ? h('a', { href: userHash(person), title: person }, shown) : shown);
    }
    const label = targetLabel ?? name ?? personLabel ?? person ?? 'an operator';
    // A labelled target keeps its slug as the tooltip, since the filters match it.
    return h('span', {}, `${kind} `, h('code', { title: targetLabel ? name : null }, label),
        name && person ? [' ', h('span', { class: 'muted small' }, personLabel ?? person)] : null);
}

// Actions whose repeats collapse into one entry per hour, with the request
// count beside it.
const COUNTED = ['catalog.admin_read', 'user.looked_up', 'provisioner.refused'];

// The details: a count for a collapsed entry, a word for a maintenance run,
// then the stored JSON (already redacted for owners), or "-" when there is none.
function detailsCell(entry) {
    const summary = COUNTED.includes(entry.action) ? count(entry.requests ?? 0, 'request')
        : entry.action === 'catalog.maintenance_run' ? 'maintenance ran'
            : null;
    const details = entry.details && Object.keys(entry.details).length
        ? h('span', { class: 'mono small' }, JSON.stringify(entry.details)) : null;
    if (!summary && !details) return h('span', { class: 'muted' }, '-');
    return h('span', {}, summary, summary && details ? ' ' : null, details);
}

function columns(admin) {
    return [
        { key: 'at', label: 'When', render: e => h('span', { title: when(e.at) }, ago(e.at)) },
        { key: 'action', label: 'Action', render: e => h('code', {}, e.action) },
        { key: 'actor', label: 'Actor', render: actorCell },
        { key: 'target', label: 'Target', render: e => targetCell(e, admin) },
        { key: 'teams', label: 'Teams', render: e => (e.teams.length ? e.teams.join(', ') : h('span', { class: 'muted' }, '-')) },
        { key: 'details', label: 'Details', render: detailsCell },
    ];
}

// A datetime-local value (the browser's local time) as UTC RFC 3339, the
// form the server parses; an empty field is no filter.
function utc(value) {
    if (!value) return '';
    const time = new Date(value);
    return Number.isNaN(time.getTime()) ? value : time.toISOString();
}

export async function render(main) {
    const admin = canAdministerPeople();
    if (!admin && !store.session) {
        main.append(h('h1', {}, 'Audit'), h('p', { class: 'muted', id: 'audit-denied' }, DENIED));
        return;
    }
    // A team owner needs its teams before it can ask for anything (the
    // server reads an owner's log one team at a time).
    let owned = [];
    if (!admin) {
        try {
            owned = (await freshAccount()).owned_teams;
        } catch {
            if (main.isConnected) {
                main.append(h('h1', {}, 'Audit'),
                    h('p', { class: 'error', id: 'audit-error' }, 'Your teams could not be read. Reload the page to try again.'));
            }
            return;   // toasted
        }
        if (!main.isConnected) return;
        if (!owned.length) {
            fill(main, h('h1', {}, 'Audit'), h('p', { class: 'muted', id: 'audit-no-team' }, NO_TEAM));
            return;
        }
    }

    const inputs = {
        team: admin
            ? h('input', { type: 'search', id: 'audit-team', name: 'team', placeholder: 'any team', autocomplete: 'off' })
            : h('select', { id: 'audit-team', name: 'team' }, owned.map(name => h('option', { value: name }, name))),
        action: h('select', { id: 'audit-action', name: 'action' }, h('option', { value: '' }, 'Any action')),
        actor: h('input', { type: 'search', id: 'audit-actor', name: 'actor', placeholder: 'person, key or label', autocomplete: 'off' }),
        target: h('input', { type: 'search', id: 'audit-target', name: 'target', placeholder: 'name or person', autocomplete: 'off' }),
        since: h('input', { type: 'datetime-local', id: 'audit-since', name: 'since' }),
        until: h('input', { type: 'datetime-local', id: 'audit-until', name: 'until' }),
    };
    const LABELS = { team: 'Team', action: 'Action', actor: 'Actor', target: 'Target', since: 'Since', until: 'Until' };
    const form = h('form', { id: 'audit-filters', class: 'audit-filters' },
        FILTERS.map(name => h('label', {}, LABELS[name], inputs[name])));
    // Enter in a text field commits it (`change`) rather than submitting.
    form.addEventListener('submit', event => event.preventDefault());
    for (const input of Object.values(inputs)) input.addEventListener('change', () => load());

    // The query the filters spell now, as text.
    function query() {
        const params = new URLSearchParams();
        for (const name of FILTERS) {
            const raw = inputs[name].value.trim();
            const value = name === 'since' || name === 'until' ? utc(raw) : raw;
            if (value) params.set(name, value);
        }
        return params.toString();
    }

    // What is drawn: the rows, the filters they answer and the cursor that
    // continues them. Only a first page that draws replaces the three, and
    // only a continuation that appends advances the cursor; a failed or
    // overtaken read leaves them as they are, so the rows on screen keep
    // their Load more (a pending read only hides the button).
    let rows = [];
    let shownQuery = null;
    let cursor = null;
    let loads = 0;           // the number of the newest table load
    let settled = 0;         // the number of the newest load that answered
    let continuing = null;   // the load whose rows a continuation is pending for
    let loadFailed = false;  // the newest load failed over the rows drawn
    let moreFailed = false;  // the last continuation of the rows drawn failed
    let actionsShown = false;

    const table = listTable(columns(admin), rows, { empty: 'No entries match.' });
    table.id = 'audit-table';
    const more = h('button', { type: 'button', id: 'audit-more', class: 'secondary', hidden: true, onclick: () => loadMore() }, 'Load more');
    const status = h('div', { id: 'audit-status' });

    // Load more shows while the rows drawn have a cursor and nothing is
    // pending: no newer load (whose rows would replace them) and no
    // continuation of these rows (a second click would append it twice).
    function showMore() {
        more.hidden = cursor === null || settled !== loads || continuing === loads;
    }

    // The reason the last read or export failed (or nothing), and the stale
    // mark while a read of the rows drawn failed. fill() drops the null
    // children (issue #302).
    function showStatus(error) {
        fill(status,
            error ? h('p', { class: 'error small', id: 'audit-error', role: 'alert' }, error.message || 'The audit log could not be read.') : null,
            (loadFailed || moreFailed) && rows.length ? h('p', { class: 'error small', id: 'audit-stale', role: 'status' }, 'The entries below may be out of date.') : null);
    }

    // The action filter's options, once, from the first answer's list.
    function showActions(actions) {
        if (actionsShown) return;
        actionsShown = true;
        const chosen = inputs.action.value;
        fill(inputs.action, h('option', { value: '' }, 'Any action'),
            actions.map(name => h('option', { value: name }, name)));
        inputs.action.value = chosen;
    }

    // A new first page for the filters as they are now. While it is pending,
    // Load more is hidden; if it fails, the rows drawn keep their cursor.
    async function load() {
        const mine = ++loads;
        const filters = query();
        showMore();
        let page;
        try {
            page = await api.get(`/audit?${filters}`, { quiet: true });
        } catch (error) {
            if (mine !== loads || !main.isConnected) return;   // overtaken
            settled = mine;
            loadFailed = true;
            showMore();
            showStatus(error);
            return;
        }
        if (mine !== loads || !main.isConnected) return;   // a newer load overtook this one
        settled = mine;
        rows = page.entries;
        shownQuery = filters;
        // A full last page is followed by an empty one, so an empty answer
        // ends the list whatever its cursor.
        cursor = page.entries.length ? page.next_after : null;
        loadFailed = false;
        moreFailed = false;
        showActions(page.actions);
        table.redraw(rows);
        showStatus(null);
        showMore();
    }

    // The next page of the rows drawn, for their filters (not the fields as
    // they may have been edited since). `after` goes first, so every query
    // for the same filters ends the same way. The answer appends only while
    // no newer load has started; a failure keeps the cursor for a retry.
    async function loadMore() {
        if (cursor === null || settled !== loads || continuing === loads) return;
        const mine = loads;
        const after = new URLSearchParams({ after: String(cursor) });
        continuing = mine;
        showMore();
        let page;
        try {
            page = await api.get(`/audit?${after}${shownQuery ? `&${shownQuery}` : ''}`, { quiet: true });
        } catch (error) {
            if (mine !== loads || !main.isConnected) return;
            continuing = null;
            moreFailed = true;
            showMore();
            showStatus(error);
            return;
        }
        if (mine !== loads || !main.isConnected) return;
        continuing = null;
        rows = rows.concat(page.entries);
        cursor = page.entries.length ? page.next_after : null;
        moreFailed = false;
        table.redraw(rows);
        showStatus(null);
        showMore();
    }

    // The export downloads every entry the filters select, with the
    // console's own credential. Its refusal stays on screen like a load's,
    // without marking the rows out of date.
    async function exportLog() {
        try {
            await api.download(`/audit/export?${query()}`, 'audit.jsonl', { quiet: true });
        } catch (error) {
            if (main.isConnected) showStatus(error);
        }
    }

    main.append(
        h('header', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Audit'),
                h('p', { class: 'muted' }, admin
                    ? 'Administrative changes, catalog writes, exports and admin reads, newest first. Sign-ins are listed in each user\'s drawer.'
                    : 'What happened in a team you own, newest first. Operator staff show as "an operator".')),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'audit-refresh', class: 'secondary', onclick: () => load() }, 'Refresh'),
                h('button', { type: 'button', id: 'audit-export', class: 'secondary', onclick: exportLog }, 'Export'))),
        form, status, table, more);
    await load();
}
