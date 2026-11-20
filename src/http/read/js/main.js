// The reader's boot and router (reader design). It registers the worker
// and waits for it to control the page, checks the session, then renders
// the screen the hash names. (Which account the cached copies belong to is
// the worker's decision, sw.js `admit`.) Every ReadError a screen throws
// comes back here, to a refusals.js handler, the one place that decides
// what a refusal does.
import { $, h, clear } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, signOut, ReadError, KIND } from './api.js';
import { refusals } from './refusals.js';
import { wipe, evictCatalog, evictDocument } from './cache.js';
import { parse, hrefs, parentOf } from './route.js';
import { message } from './parts.js';
import * as home from './screens/home.js';
import * as catalogs from './screens/catalogs.js';
import * as tree from './screens/tree.js';
import * as search from './screens/search.js';
import * as concept from './screens/concept.js';
// Not `document`: that name would hide the browser's global in this module.
import * as documentScreen from './screens/document.js';
import * as me from './screens/me.js';

const SCREENS = { home, catalogs, tree, search, concept, document: documentScreen, me };
const NEEDS_CATALOG = new Set(['home']);
const CATALOG_KEY = 'okf.read.catalog';

const state = { session: null, catalog: remembered() };
// Bumped on every navigation, so a late answer never draws over a newer screen.
let generation = 0;
// True while the page is leaving this account (a sign-out under way, or a
// 401 on its way to sign-in): no catalog is remembered meanwhile, so one
// chosen during the wipe or the DELETE never outlives the account.
let leaving = false;

function remembered() {
    try { return localStorage.getItem(CATALOG_KEY); } catch { return null; }
}

// Stores `catalog` as the remembered one (null deletes it).
function remember(catalog) {
    try {
        if (catalog) localStorage.setItem(CATALOG_KEY, catalog);
        else localStorage.removeItem(CATALOG_KEY);
    } catch { /* storage blocked: the choice lasts for this visit only */ }
}

// Makes `catalog` the current one (null forgets it). While the page is
// leaving the account it is current for this page only, never remembered.
function choose(catalog) {
    state.catalog = catalog;
    remember(leaving ? null : catalog);
    tabs();
}

function signInUrl() {
    return `/signin?return_to=${encodeURIComponent(`/read/${location.hash}`)}`;
}

// Deletes everything this device keeps of the account: every offline copy
// and the remembered catalog, so the next person to sign in here never sees
// the last one's catalog name. Sign-out, "Delete offline copies" and a 401
// all go through here.
async function forget() {
    choose(null);
    await wipe();
}

// Leaves the account for the sign-in page at `url`, deleting the
// remembered catalog once more on the way out.
function leaveFor(url) {
    remember(null);
    location.assign(url);
}

async function signIn() {
    leaving = true;
    await forget();
    leaveFor(signInUrl());
}

// Signs out (the account screen's button): deletes the offline copies and
// the remembered catalog, then ends the session and leaves for sign-in.
// Returns false, keeping the session, when the server could not be reached
// or refused; catalogs are remembered again from then on.
async function endSession(csrf) {
    leaving = true;
    await forget();
    try {
        if (await signOut(csrf)) {
            leaveFor(`/signin?return_to=${encodeURIComponent('/read/')}`);
            return true;
        }
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
    }
    leaving = false;
    return false;
}

// Shows the offline banner when `result` came from the worker's cache.
function freshness(result) {
    if (!result?.offline) return;
    const banner = $('#offline-banner');
    banner.textContent = `Offline. Synced ${ago(result.cachedAt)}.`;
    banner.hidden = false;
}

function tabs() {
    const current = parse(location.hash).screen;
    const catalog = state.catalog;
    $('#tab-browse').setAttribute('href', catalog ? hrefs.tree(catalog) : hrefs.catalogs());
    $('#tab-search').setAttribute('href', catalog ? hrefs.search(catalog) : hrefs.catalogs());
    $('#bar-catalog').textContent = catalog ?? 'Reader';
    const active = { home: 'tab-home', tree: 'tab-browse', concept: 'tab-browse', document: 'tab-browse', search: 'tab-search', me: 'tab-me' }[current];
    for (const tab of document.querySelectorAll('.tabs a')) {
        if (tab.id === active) tab.setAttribute('aria-current', 'page');
        else tab.removeAttribute('aria-current');
    }
}

// Draws a refusal's notice on `view` (the screen at `route`); refusals.js
// decides when, and which refusals draw something else.
function paint(view, route, error) {
    switch (error.kind) {
        case KIND.CONCEPT_GONE:
            clear(view).append(message('This concept no longer exists.',
                h('a', { href: hrefs.tree(route.catalog, parentOf(route.path)), id: 'gone-folder' }, 'Open its folder'), 'concept-gone'));
            return;
        case KIND.LOCKED:
            clear(view).append(message(error.message,
                error.body?.error === 'UnlockRequired' ? h('a', { href: '/ui/', id: 'unlock-link' }, 'Unlock in the console') : null, 'locked'));
            return;
        case KIND.UNAVAILABLE:
            clear(view).append(message('Not available offline.',
                h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'unavailable'));
            return;
        default:
            clear(view).append(message(error.message, null, 'failed'));
    }
}

async function show() {
    const parsed = parse(location.hash);
    const mine = ++generation;
    if ((parsed.catalog ?? null) !== null && parsed.catalog !== state.catalog) choose(parsed.catalog);
    // The catalog this screen reads, fixed now: a refusal that answers after
    // the user switched catalogs must act on the one that was asked about.
    const route = { ...parsed, catalog: parsed.catalog ?? state.catalog };
    tabs();
    $('#offline-banner').hidden = true;
    if (NEEDS_CATALOG.has(route.screen) && !route.catalog) {
        location.replace(hrefs.catalogs());
        return;
    }
    const view = h('section', { class: 'screen', id: `screen-${route.screen}` });
    clear($('#reader')).append(view);
    const live = () => mine === generation;
    const refusal = refusals(route, live, {
        signIn, evictCatalog, evictDocument, choose,
        current: () => state.catalog,
        go: hash => { location.hash = hash; },
        paint: error => paint(view, route, error),
    });
    const ctx = {
        state, route, choose, forget, signOut: endSession, freshness, live,
        fail: refusal.fail,
        settle: refusal.settle,
        drawWorst: refusal.drawWorst,
    };
    try {
        await SCREENS[route.screen].render(view, ctx);
    } catch (error) {
        await refusal.caught(error);
    }
}

// A warning, not an error: the reader still works online.
function registrationFailed(error) {
    console.warn('the reader\'s worker could not be registered:', error?.message);
}

// How long a first visit waits for the new worker to take control.
const WORKER_WAIT_MS = 5000;

// Registers the worker and resolves once it controls this page, so the
// first screen's reads already go through it and are cached (on a first
// visit, a direct concept link included). It resolves anyway, without a
// worker, when registration fails or takes too long: the reader then works
// online and only offline reading is lost.
function registerWorker() {
    if (!('serviceWorker' in navigator)) return Promise.resolve();
    const workers = navigator.serviceWorker;
    if (workers.controller) {
        // Already controlled: this checks for a newer build in the
        // background, and the worker it has keeps serving if that fails.
        workers.register('/read/sw.js', { scope: '/read/' }).catch(registrationFailed);
        return Promise.resolve();
    }
    return new Promise(resolve => {
        const timer = setTimeout(resolve, WORKER_WAIT_MS);
        const done = () => { clearTimeout(timer); resolve(); };
        // sw.js calls skipWaiting and clients.claim, so a first install
        // takes this page over and fires controllerchange.
        workers.addEventListener('controllerchange', done, { once: true });
        workers.register('/read/sw.js', { scope: '/read/' }).catch(error => {
            registrationFailed(error);
            done();
        });
    });
}

async function boot() {
    await registerWorker();
    try {
        const { data } = await read('/session');
        state.session = data;
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        if (error.kind === KIND.SIGNED_OUT) { await signIn(); return; }
        // Offline (or the server failing): read what is cached; the
        // banner says so on the first cached answer.
    }
    $('#reader').hidden = false;
    window.addEventListener('hashchange', show);
    await show();
}

boot();
