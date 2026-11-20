// View parts more than one screen uses.
import { h } from '/ui/js/core/dom.js';
import { fromBrowse, hrefs } from './route.js';

// The search box, which opens the search screen for `catalog`.
export function searchForm(catalog, query = '') {
    const input = h('input', { type: 'search', name: 'q', id: 'search-input', placeholder: 'Search', value: query, 'aria-label': 'Search', enterkeyhint: 'search' });
    return h('form', {
        id: 'search-form', role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            if (text) location.hash = hrefs.search(catalog, text);
        },
    }, input);
}

// Breadcrumbs from the catalog root to the folder holding `path` (or to
// `path` itself when it is a folder).
export function crumbs(catalog, path, isFolder) {
    const parts = String(path ?? '').split('/').filter(Boolean);
    const folders = isFolder ? parts : parts.slice(0, -1);
    const items = [h('li', {}, h('a', { href: hrefs.tree(catalog) }, 'Contents'))];
    folders.forEach((name, at) => {
        items.push(h('li', {}, h('a', { href: hrefs.tree(catalog, folders.slice(0, at + 1).join('/')) }, name)));
    });
    return h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, h('ol', {}, items));
}

// body_html from src/render.rs, made the reader's: links to the console's
// browse routes go to reader routes, web links open outside the app.
export function renderBody(html) {
    const article = h('article', { class: 'markdown', id: 'body' });
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
