// The reader's boot and router (reader design). It registers the worker
// and waits for it to control the page, checks the session, then renders
// the screen the hash names. (Which account the cached copies belong to is
// the worker's decision, sw.js `admit`.) Every ReadError a screen throws
// comes back here, to effects() and draw(), the one place that decides
// what a refusal does.
import { $, h, clear } from '/ui/js/core/dom.js';
import { ago } from '/ui/js/core/format.js';
import { read, ReadError, KIND } from './api.js';
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

function remembered() {
    try { return localStorage.getItem(CATALOG_KEY); } catch { return null; }
}

// Makes `catalog` the current one (null forgets it).
function choose(catalog) {
    state.catalog = catalog;
    try {
        if (catalog) localStorage.setItem(CATALOG_KEY, catalog);
        else localStorage.removeItem(CATALOG_KEY);
    } catch { /* storage blocked: the choice lasts for this visit only */ }
    tabs();
}

function signInUrl() {
    return `/signin?return_to=${encodeURIComponent(`/read/${location.hash}`)}`;
}

async function signIn() {
    await wipe();
    location.assign(signInUrl());
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

// What a refusal changes, whichever screen shows now: a 401 or a lost
// grant is true even when the screen that asked has been left. Run as soon
// as that one request is refused, never after its siblings.
async function effects(route, error) {
    switch (error.kind) {
        case KIND.SIGNED_OUT:
            await signIn();
            return;
        case KIND.CATALOG_GONE:
            if (route.catalog) await evictCatalog(route.catalog);
            if (route.catalog === state.catalog) choose(null);
            return;
        case KIND.CONCEPT_GONE:
            await evictDocument(route.catalog, route.screen === 'document' ? 'documents' : 'concepts', route.path);
            return;
        case KIND.LOCKED:
        case KIND.UNAVAILABLE:
        case KIND.FAILED:
            return;
    }
}

// What a refusal shows, on the screen that asked, while it is still shown.
function draw(view, route, error, live) {
    if (!live()) return;
    switch (error.kind) {
        case KIND.SIGNED_OUT:
            return; // effects() has already left for sign-in.
        case KIND.CATALOG_GONE:
            location.hash = hrefs.catalogs(route.catalog);
            return;
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
        case KIND.FAILED:
            clear(view).append(message(error.message, null, 'failed'));
            return;
    }
}

// The kinds from most to least serious: when several reads of one screen
// are refused, the most serious one alone draws.
const SEVERITY = [KIND.SIGNED_OUT, KIND.CATALOG_GONE, KIND.CONCEPT_GONE, KIND.LOCKED, KIND.FAILED, KIND.UNAVAILABLE];

function worst(errors) {
    return [...errors].sort((a, b) => SEVERITY.indexOf(a.kind) - SEVERITY.indexOf(b.kind))[0];
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
    const ctx = {
        state, route, choose, freshness, live,
        // A single refused read (a screen's, or a button's later): its
        // effects, then its drawing.
        fail: async error => { await effects(route, error); draw(view, route, error, live); },
        // For a screen that makes several reads at once: `settle` runs each
        // read's refusal effects the moment it is refused (marking the
        // error handled), and `drawWorst` draws once all have settled.
        settle: promise => promise.catch(async error => {
            if (error instanceof ReadError) {
                error.handled = true;
                await effects(route, error);
            }
            throw error;
        }),
        drawWorst: errors => draw(view, route, worst(errors), live),
    };
    try {
        await SCREENS[route.screen].render(view, ctx);
    } catch (error) {
        if (!(error instanceof ReadError)) throw error;
        if (error.handled) draw(view, route, error, live);
        else await ctx.fail(error);
    }
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
        workers.register('/read/sw.js', { scope: '/read/' }).catch(() => {});
        return Promise.resolve();
    }
    return new Promise(resolve => {
        const timer = setTimeout(resolve, WORKER_WAIT_MS);
        const done = () => { clearTimeout(timer); resolve(); };
        // sw.js calls skipWaiting and clients.claim, so a first install
        // takes this page over and fires controllerchange.
        workers.addEventListener('controllerchange', done, { once: true });
        workers.register('/read/sw.js', { scope: '/read/' }).catch(error => {
            console.warn('offline reading is unavailable:', error.message);
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
