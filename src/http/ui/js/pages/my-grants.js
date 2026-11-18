// My grants: the OAuth grants that let a client (a chat assistant, say)
// act as the signed-in person: those from consent signed in with the
// account, capped at the ceiling, and those from an admin's sign-in code.
// Each grant's drawer revokes it; the client is refused from its next
// request. Last active is when the grant's newest token was issued (each
// sign-in and refresh issues one); the server revokes a grant idle for
// oauth.idle_grant_revoke_after (30 days by default). The ceiling the cap
// notes compare with is the one /me gives on this load, never the sign-in
// snapshot.
import { h, pill, fact } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { catalogLabel } from '../core/labels.js';
import { toast } from '../core/toast.js';
import { drawerTicket } from '../core/drawer.js';
import { when } from '../core/format.js';
import { freshAccount, requireSession } from '../core/shell.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { levelsCell } from '../components/levels.js';

// Where a grant came from.
const ORIGIN = grant => grant.self_issued ? 'your consent' : "an admin's sign-in code";

// A grant's client, as the page names it.
const CLIENT = grant => grant.client_name ?? 'unknown client';

const COLUMNS = [
    { key: 'client_name', label: 'Client', sort: grant => grant.client_name ?? '',
      render: grant => h('span', { class: 'key-name', dataset: { test: `my-grant-${grant.name}` } }, CLIENT(grant)) },
    { key: 'catalogs', label: 'Catalogs', render: grant => levelsCell(grant, store.session?.ceiling) },
    { key: 'default_catalog', label: 'Default', render: row => (row.default_catalog ? catalogLabel(row.default_catalog) : '-') },
    { key: 'self_issued', label: 'From', render: ORIGIN },
    { key: 'created_at', label: 'Approved', sort: grant => grant.created_at, render: grant => when(grant.created_at) },
    { key: 'last_activity', label: 'Last active', sort: grant => grant.last_activity ?? '',
      render: grant => h('span', { dataset: { test: `my-grant-active-${grant.name}` } }, when(grant.last_activity)) },
];

export async function render(main) {
    main.append(h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'My grants'),
            h('p', { class: 'muted' }, 'OAuth clients you connected to your catalogs. Revoke one to disconnect it.'))));
    if (!requireSession(main, 'my-grants-denied')) return;
    const table = listTable(COLUMNS, [], { onRow: grant => showGrant(main, grant, load), empty: 'No OAuth grants.' });
    table.id = 'my-grants-table';
    // Revoked grants authenticate nothing; they stay in the admin's list.
    table.setFilter(grant => !grant.revoked_at);
    main.append(table);

    async function load() {
        let grants;
        try {
            // freshAccount() makes the session's ceiling the one /me gives
            // now, which the cap notes read.
            [, { grants }] = await Promise.all([freshAccount(), api.get('/me/grants')]);
        } catch { return; /* toasted */ }
        if (main.isConnected) table.redraw(grants);
    }
    await load();
}

function showGrant(main, grant, reload) {
    const ticket = drawerTicket(main);
    const revoke = h('button', {
        type: 'button', id: 'my-grant-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Disconnect ${CLIENT(grant)}?`,
                message: 'The client is refused from its next request, and its tokens stop working. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try { await api.del(`/me/grants/${enc(grant.id)}`); } catch { return; /* toasted */ }
            toast(`Revoked the grant for ${CLIENT(grant)}.`, 'ok');
            ticket.close();
            if (main.isConnected) await reload();
        },
    }, 'Revoke');
    ticket.open(CLIENT(grant), 'OAuth grant',
        h('dl', { class: 'facts' },
            fact('Id', h('code', {}, grant.id)),
            fact('Catalogs', levelsCell(grant, store.session?.ceiling)),
            fact('Default', grant.default_catalog ? catalogLabel(grant.default_catalog) : '-'),
            fact('From', ORIGIN(grant)),
            fact('Approved', when(grant.created_at)),
            fact('Last active', when(grant.last_activity)),
            fact('State', pill('active', 'ok'))),
        h('div', { class: 'drawer-actions' }, revoke));
}
