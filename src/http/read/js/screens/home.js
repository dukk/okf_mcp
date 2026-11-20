// Home: the search box, recent changes in the current catalog, and, when
// the server cannot be reached, what this device can read offline. On an
// iPhone's browser (not yet installed) it says how to install.
import { h, fill } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, KIND } from '../api.js';
import { offlineDocuments } from '../cache.js';
import { hrefFor } from '../route.js';
import { searchForm, message } from '../parts.js';

const HINT_KEY = 'okf.read.install-hint';

// The install hint, or null when it does not apply (not an iPhone or iPad
// browser, already installed, or dismissed before).
function installHint() {
    const ios = /iPad|iPhone|iPod/.test(navigator.userAgent)
        || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
    let dismissed = false;
    try { dismissed = localStorage.getItem(HINT_KEY) === 'dismissed'; } catch { /* storage blocked */ }
    if (!ios || navigator.standalone || dismissed) return null;
    const close = h('button', { type: 'button', class: 'secondary outline' }, 'Got it');
    const hint = message('To install: tap Share, then Add to Home Screen.', close, 'install-hint');
    close.addEventListener('click', () => {
        try { localStorage.setItem(HINT_KEY, 'dismissed'); } catch { /* storage blocked */ }
        hint.remove();
    });
    return hint;
}

// Appends the "Available offline" section for `items` (offlineDocuments).
function drawOffline(view, catalog, items) {
    view.append(h('h2', {}, 'Available offline'));
    if (!items.length) {
        view.append(h('p', { class: 'hint', id: 'offline-list' }, 'Nothing yet. Concepts you open are kept for reading offline.'));
        return;
    }
    view.append(h('ul', { class: 'list', id: 'offline-list' }, items.map(item => h('li', {},
        h('a', { href: hrefFor(catalog, item.path) }, item.title ?? item.path,
            h('span', { class: 'meta' }, `Synced ${ago(item.cachedAt)}`))))));
}

export async function render(view, ctx) {
    const catalog = ctx.route.catalog;
    // fill, not append: the hint is null except on an iPhone's browser.
    fill(view, installHint(), searchForm(catalog));
    let result;
    try {
        result = await read(`/catalogs/${enc(catalog)}/recent-changes?limit=20`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        const items = await offlineDocuments(catalog);
        if (ctx.live()) drawOffline(view, catalog, items);
        return;
    }
    // After an await the user may have left: draw nothing, touch no banner.
    const items = result.offline ? await offlineDocuments(catalog) : null;
    if (!ctx.live()) return;
    ctx.freshness(result);
    view.append(h('h2', {}, 'Recent changes'), h('ul', { class: 'list', id: 'recent' }, result.data.changes.map(change =>
        h('li', {}, change.op === 'delete'
            ? h('span', {}, change.path, h('span', { class: 'meta' }, `Deleted ${ago(change.at)}`))
            : h('a', { href: hrefFor(catalog, change.path) }, change.path,
                h('span', { class: 'meta' }, `${change.message || change.op} - ${change.actor_label ?? change.actor}, ${ago(change.at)}`))))));
    if (items) drawOffline(view, catalog, items);
}
