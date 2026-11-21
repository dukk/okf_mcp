// Home: the search box (with the catalog's page count), "Jump back in" (the
// concepts read last, from the offline copies), and the catalog's activity:
// recent changes by day and batch (timeline.js), with filter chips and
// "Show older". When the server cannot be reached, or the worker answered
// from its copy, every offline copy is listed instead, under "Available
// offline". On an iPhone's browser (not yet installed) it says how to install.
import { h, fill } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, enc, KIND, ReadError } from '../api.js';
import { offlineDocuments } from '../cache.js';
import { hrefFor } from '../route.js';
import { searchForm, message, row, sectionHead, pill } from '../parts.js';
import { icon } from '../icons.js';
import { FILTERS, GROUP_PREVIEW, changeKind, object, timeline, verb } from '../timeline.js';
import { avatarClass, initials } from '../avatar.js';
import { clock, docLabel, folderTrail, pages, shortAgo } from '../text.js';

const HINT_KEY = 'okf.read.install-hint';
// Changes per page: the server's default.
const PAGE = 50;
// Rows of "Jump back in".
const JUMP_BACK = 3;

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

// One offline copy ({ path, title, cachedAt }) as a row.
function copyRow(catalog, copy) {
    const where = [folderTrail(copy.path), `Synced ${ago(copy.cachedAt)}`].filter(Boolean).join(' · ');
    return row({ href: hrefFor(catalog, copy.path), glyph: 'doc', title: docLabel(copy), sub: where, end: icon('offline', 'chev') });
}

// "Jump back in" (online: the newest JUMP_BACK copies, nothing when there
// are none), or "Available offline" (`all`: every copy, or a hint).
function drawCopies(view, catalog, copies, all) {
    if (!all) {
        if (!copies.length) return;
        view.append(sectionHead('Jump back in'),
            h('div', { class: 'rows', id: 'jump-back' }, copies.slice(0, JUMP_BACK).map(copy => copyRow(catalog, copy))));
        return;
    }
    view.append(sectionHead('Available offline'));
    if (!copies.length) {
        view.append(h('p', { class: 'hint', id: 'offline-list' }, 'Nothing yet. Concepts you open are kept for reading offline.'));
        return;
    }
    view.append(h('div', { class: 'rows', id: 'offline-list' }, copies.map(copy => copyRow(catalog, copy))));
}

// One document of a group: its title (struck through and not a link when
// deleted), its folder, and a pill for what happened to it.
function itemRow(catalog, change) {
    const kind = changeKind(change);
    return row({
        href: kind === 'deleted' ? null : hrefFor(catalog, change.path),
        glyph: 'doc', title: docLabel(change), sub: folderTrail(change.path) || null,
        end: pill(kind, kind), cls: kind === 'deleted' ? 'deleted' : null,
    });
}

// One group: "Name imported 6 pages" with its documents (the first
// GROUP_PREVIEW shown, then "+N more"), or, for one change, a single row
// that opens the document (not for a deletion).
function groupView(catalog, group, today) {
    const first = group.items[0];
    const single = group.items.length === 1;
    const deleted = single && changeKind(first) === 'deleted';
    const linked = single && !deleted;
    const who = group.actorLabel ?? group.actor;
    const head = h(linked ? 'a' : 'div', {
        class: deleted ? 'group-head deleted' : 'group-head',
        href: linked ? hrefFor(catalog, first.path) : null,
    },
    h('span', { class: `avatar ${avatarClass(group.actor)}`, 'aria-hidden': 'true' }, initials(who)),
    h('span', { class: 'who' }, h('b', {}, who), ` ${verb(group)} `, h('span', { class: 'object' }, object(group))),
    h('span', { class: 'when' }, today ? shortAgo(group.at) : clock(group.at)));
    if (single) return h('div', { class: 'group' }, head);
    const items = group.items.map(change => itemRow(catalog, change));
    items.slice(GROUP_PREVIEW).forEach(item => { item.hidden = true; });
    const rest = items.length - GROUP_PREVIEW;
    const more = rest > 0 ? h('button', { type: 'button', class: 'group-more' }, `+${rest} more`) : null;
    more?.addEventListener('click', () => {
        for (const item of items) item.hidden = false;
        more.remove();
    });
    return h('div', { class: 'group' }, head, h('div', { class: 'items' }, items, more));
}

// The activity section: chips, the timeline of every page read so far, and
// "Show older" while the server has more.
function drawActivity(view, ctx, catalog, first) {
    const me = ctx.state.session?.user ?? null;
    const base = `/catalogs/${enc(catalog)}/recent-changes?limit=${PAGE}`;
    const loaded = { changes: [...first.data.changes], next: first.data.next_after ?? null, filter: 'all' };
    const list = h('div', { class: 'timeline', id: 'recent' });
    const older = h('button', { type: 'button', class: 'more-button', id: 'recent-older' }, 'Show older');
    const chips = FILTERS.map(filter => h('button', {
        type: 'button', class: 'chip', dataset: { filter: filter.id },
        onclick: () => { loaded.filter = filter.id; redraw(); },
    }, filter.label));
    function redraw() {
        for (const chip of chips) chip.setAttribute('aria-pressed', String(chip.dataset.filter === loaded.filter));
        older.hidden = loaded.next === null;
        const days = timeline(loaded.changes, { filter: loaded.filter, me, now: new Date() });
        if (!days.length) {
            fill(list, h('p', { class: 'hint', id: 'recent-empty' }, loaded.changes.length ? 'No changes match.' : 'No changes yet.'));
            return;
        }
        fill(list, days.map(day => [
            h('h3', { class: 'day' }, day.label),
            day.groups.map(group => groupView(catalog, group, day.label === 'Today')),
        ]));
    }
    older.addEventListener('click', async () => {
        older.disabled = true;
        try {
            const page = await read(`${base}&after=${enc(loaded.next)}`);
            // The user may have left meanwhile: touch neither the banner nor a newer screen.
            if (!ctx.live()) return;
            ctx.freshness(page);
            // A retry that works makes the earlier "needs a connection" stale.
            view.querySelector('#older-offline')?.remove();
            loaded.changes.push(...page.data.changes);
            const next = page.data.next_after ?? null;
            // A cursor that does not move would never end: stop at it.
            loaded.next = next === loaded.next ? null : next;
            redraw();
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
            if (error.kind !== KIND.UNAVAILABLE) {
                // A 401 or a lost grant here wipes or evicts like any other read.
                await ctx.fail(error);
                return;
            }
            if (ctx.live() && !document.getElementById('older-offline')) {
                older.after(message('Older changes need a connection.', null, 'older-offline'));
            }
        } finally {
            older.disabled = false;
        }
    });
    view.append(sectionHead('Activity'),
        h('div', { class: 'chips', role: 'group', 'aria-label': 'Show changes' }, chips),
        list, older);
    redraw();
}

export async function render(view, ctx) {
    const catalog = ctx.route.catalog;
    const box = searchForm(catalog);
    // fill, not append: the hint is null except on an iPhone's browser.
    fill(view, installHint(), box);
    // The page count comes with the catalog list (main.js); the box works without it.
    ctx.summary().then(known => {
        if (known && ctx.live()) box.querySelector('input').placeholder = `Search ${pages(known.concepts)}`;
    });
    let result;
    try {
        result = await read(`/catalogs/${enc(catalog)}/recent-changes?limit=${PAGE}`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        const copies = await offlineDocuments(catalog);
        if (ctx.live()) drawCopies(view, catalog, copies, true);
        return;
    }
    const copies = await offlineDocuments(catalog);
    // After an await the user may have left: draw nothing, touch no banner.
    if (!ctx.live()) return;
    ctx.freshness(result);
    drawCopies(view, catalog, copies, result.offline);
    drawActivity(view, ctx, catalog, result);
}
