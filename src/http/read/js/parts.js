// View parts more than one screen uses. Frozen once the screens are built
// on them: a screen that needs a new part keeps it in its own file.
import { h } from '/ui/js/core/dom.js';
import { fromBrowse, hrefs } from './route.js';
import { icon } from './icons.js';

// The search box, which opens the search screen for `catalog`.
export function searchForm(catalog, query = '', placeholder = 'Search') {
    const input = h('input', { type: 'search', name: 'q', id: 'search-input', placeholder, value: query, 'aria-label': 'Search', enterkeyhint: 'search' });
    return h('form', {
        id: 'search-form', role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            if (text) location.hash = hrefs.search(catalog, text);
        },
    }, icon('search'), input);
}

// Breadcrumbs from the catalog root to the folder holding `path` (or to
// `path` itself when it is a folder). `root` names the catalog (its label;
// its name by default); `titles` (a Map of folder path to title, from the
// folders' indexes) names the folders that have a title.
export function crumbs(catalog, path, isFolder, { root = catalog, titles = null } = {}) {
    const parts = String(path ?? '').split('/').filter(Boolean);
    const folders = isFolder ? parts : parts.slice(0, -1);
    // Marked with its catalog's name, so main.js relabels it if the catalog
    // list is read after this screen is drawn.
    const items = [h('li', {}, h('a', { href: hrefs.tree(catalog), 'data-catalog-label': catalog }, root))];
    folders.forEach((name, at) => {
        const folder = folders.slice(0, at + 1).join('/');
        items.push(h('li', {}, h('a', { href: hrefs.tree(catalog, folder) }, titles?.get(folder) ?? name)));
    });
    return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, h('ol', {}, items));
}

// body_html from src/render.rs, made the reader's: links to the console's
// browse routes go to reader routes, web links open outside the app.
// `serif` sets the concept reading type.
export function renderBody(html, { serif = false } = {}) {
    const article = h('article', { class: serif ? 'markdown serif' : 'markdown', id: 'body' });
    // The reader's only innerHTML, as in the console: render.rs writes
    // body_html as safe HTML (no raw HTML, no unsafe link schemes).
    article.innerHTML = html;
    for (const link of article.querySelectorAll('a[href]')) {
        const href = link.getAttribute('href');
        const route = fromBrowse(href);
        if (route) link.setAttribute('href', route);
        else if (/^https?:/i.test(href)) {
            link.setAttribute('target', '_blank');
            link.setAttribute('rel', 'noopener');
        }
    }
    return article;
}

// A short notice with an optional action (a link or button).
export function message(text, action = null, id = 'notice') {
    return h('div', { class: 'notice', id }, h('p', {}, text), action);
}

// A list row: an icon tile (`glyph`, tinted by `tint`), a title, an optional
// line under it (`sub`), an optional note of up to two lines (`note`), and
// what goes at its end (`end`: a count, a pill, an icon). A link when
// `href` is given.
export function row({ href = null, glyph = 'doc', tint = null, title, sub = null, note = null, end = null, id = null, cls = null }) {
    return h(href ? 'a' : 'div', { class: cls ? `row ${cls}` : 'row', href, id },
        h('span', { class: tint ? `ic ic-${tint}` : 'ic' }, icon(glyph)),
        h('span', { class: 'tx' },
            h('span', { class: 't' }, title),
            sub ? h('span', { class: 's' }, sub) : null,
            note ? h('span', { class: 'n' }, note) : null),
        end);
}

// A section heading ('Pages · 42' with `count`).
export function sectionHead(title, { count = null, id = null } = {}) {
    return h('div', { class: 'sec', id }, h('h2', {}, count === null ? title : `${title} · ${count}`));
}

// A pill; `kind` is new, edited, deleted or type (read.css .pill-<kind>).
export function pill(text, kind = null) {
    return h('span', { class: kind ? `pill pill-${kind}` : 'pill' }, text);
}

// A screen's title.
export function pageTitle(text, id = null) {
    return h('h1', { class: 'page', id }, text);
}

// A lede paragraph, or null without text (h() and fill() drop it).
export function lede(text, cls = 'lede') {
    return text ? h('p', { class: cls }, text) : null;
}
