// Account: who is signed in, until when, sign-out (which deletes every
// offline copy first, even if the server cannot be reached), and a local
// "Delete offline copies" that works with no connection at all.
import { h } from '/ui/js/core/dom.js';
import { when } from '/ui/js/core/format.js';
import { signOut, ReadError } from '../api.js';
import { wipe } from '../cache.js';
import { message } from '../parts.js';

function deleteOffline(view) {
    const button = h('button', { type: 'button', class: 'secondary', id: 'delete-offline' }, 'Delete offline copies');
    button.addEventListener('click', async () => {
        button.disabled = true;
        await wipe();
        view.append(message('Offline copies are deleted from this device.', null, 'offline-deleted'));
    });
    return button;
}

export async function render(view, ctx) {
    const session = ctx.state.session;
    view.append(h('h1', {}, 'Account'));
    if (!session) {
        view.append(message('Account details and sign-out need a connection.', null, 'account-offline'), deleteOffline(view));
        return;
    }
    const button = h('button', { type: 'button', id: 'sign-out' }, 'Sign out');
    button.addEventListener('click', async () => {
        button.disabled = true;
        await wipe();
        try {
            if (await signOut(session.csrf)) {
                location.assign(`/signin?return_to=${encodeURIComponent('/read/')}`);
                return;
            }
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
        }
        button.disabled = false;
        view.append(message('The server could not be reached to end the session. Offline copies are deleted.', null, 'sign-out-failed'));
    });
    view.append(
        h('p', {}, 'Signed in as ', h('strong', { id: 'account-user' }, session.user)),
        session.email ? h('p', { class: 'hint' }, session.email) : null,
        h('p', { class: 'hint' }, `This session ends by ${when(session.expires_at)}.`),
        button,
        deleteOffline(view));
}
