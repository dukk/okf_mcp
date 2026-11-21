// Search in the current catalog. It needs the server: offline it says so
// and points at what is available offline.
import { h } from '/ui/js/core/dom.js';
import { count } from '/ui/js/core/format.js';
import { read, enc, KIND } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { searchForm, message } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, query } = ctx.route;
    view.append(searchForm(catalog, query));
    if (!query) return;
    let result;
    try {
        result = await read(`/search?q=${enc(query)}&catalogs=${enc(catalog)}&limit=50`);
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        if (!ctx.live()) return;
        view.append(message('Search needs a connection.',
            h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'search-offline'));
        return;
    }
    if (!ctx.live()) return;
    const hits = result.data.hits;
    view.append(h('p', { class: 'hint', id: 'search-count' }, count(hits.length, 'hit')),
        h('ul', { class: 'list', id: 'hits' }, hits.map(hit => h('li', {},
            // h() drops a null child; the native append would print "null".
            h('a', { href: hrefFor(hit.catalog, hit.path) }, hit.title ?? hit.path,
                hit.snippet ? h('span', { class: 'meta' }, hit.snippet) : null)))));
}
