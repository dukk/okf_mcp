// Search: ranked concepts with snippets. A key that reaches at most the
// server's `search_fanout_max` catalogs searches all of them (the request
// names none, so the server's own every-reachable path applies); one that
// reaches more chooses at most that many. The cap is the server's, read from
// /whoami. Route: #/search/<query>, so a search can be linked and reloaded.
import { h, clear } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { go, browseHash } from '../core/router.js';
import { count } from '../core/format.js';
import { latest } from '../core/latest.js';

// The catalogs chosen for a search past the cap; kept while the console is
// open, so a second search keeps the choice.
let chosen = null;

// The chosen catalogs, still reachable, at most `cap`: the current catalog
// first, then the first reachable ones, when nothing is chosen yet.
function initialChoice(catalogs, cap) {
    const kept = (chosen ?? []).filter(name => catalogs.includes(name)).slice(0, cap);
    if (kept.length) return kept;
    const first = store.catalog && catalogs.includes(store.catalog) ? [store.catalog] : [];
    return first.concat(catalogs.filter(name => name !== store.catalog)).slice(0, cap);
}

// A checkbox list limited to `cap` names. Once `cap` are checked the others
// are disabled, so the request never names more than the server accepts.
function chooser(catalogs, cap) {
    const picked = new Set(initialChoice(catalogs, cap));
    chosen = [...picked];
    const note = h('p', { class: 'muted small', id: 'search-cap' });
    const boxes = catalogs.map(name => h('label', { class: 'small' },
        h('input', {
            type: 'checkbox', name: 'catalog', value: name, checked: picked.has(name),
            onchange: event => {
                if (event.target.checked) picked.add(name); else picked.delete(name);
                chosen = [...picked];
                refresh();
            },
        }), ` ${name}`));
    const refresh = () => {
        boxes.forEach(label => {
            const box = label.querySelector('input');
            box.disabled = !box.checked && picked.size >= cap;
        });
        note.textContent = `${picked.size} of at most ${cap} catalogs chosen.`;
    };
    refresh();
    return h('details', { id: 'search-chooser', open: true },
        h('summary', {}, 'Choose catalogs'),
        h('p', { class: 'muted small' },
            `This key reaches ${catalogs.length} catalogs. One search covers at most ${cap}, so choose which.`),
        h('fieldset', {}, boxes),
        note);
}

export async function render(main, rest) {
    const query = rest.join('/');
    const reads = latest(main);
    const catalogs = store.whoami?.catalogs ?? [];
    const cap = store.whoami?.search_fanout_max ?? 0;
    const choosing = cap > 0 && catalogs.length > cap;
    const input = h('input', {
        type: 'search', id: 'search-query', name: 'q', value: query, autocomplete: 'off',
        placeholder: 'Words that must all occur', 'aria-label': 'Search',
    });
    const form = h('form', {
        id: 'search-form',
        role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            go(text ? `#/search/${enc(text)}` : '#/search');
        },
    }, h('fieldset', { role: 'group' }, input, h('button', { type: 'submit' }, 'Search')));
    const results = h('div', { id: 'search-results' });
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Search'),
                h('p', { class: 'muted small' }, choosing ? 'Across the catalogs chosen below.'
                    : catalogs.length ? `Across ${catalogs.join(', ')}` : 'This key reaches no catalog.'))),
        form,
        ...(choosing ? [chooser(catalogs, cap)] : []),
        results);
    input.focus();
    if (!query) return;
    results.append(h('p', { class: 'muted' }, 'Searching...'));
    // Past the cap, the chosen ones; otherwise none are named and the server
    // searches every catalog the key reaches.
    const named = choosing ? `&catalogs=${chosen.map(enc).join(',')}` : '';
    const current = reads();
    let found;
    try {
        found = await api.get(`/search?q=${enc(query)}${named}&limit=50`);
    } catch {
        if (current()) clear(results);
        return;
    }
    if (!current()) return;
    clear(results).append(
        h('p', { class: 'muted small', id: 'search-count' }, count(found.hits.length, 'hit')),
        h('ol', { class: 'hits' }, found.hits.map(hit => h('li', { class: 'hit' },
            h('a', { class: 'hit-title', href: browseHash(hit.catalog, hit.path), dataset: { catalog: hit.catalog, path: hit.path } },
                hit.title ?? hit.path),
            h('span', { class: 'mono small muted' }, ` ${hit.catalog} ${hit.path}`),
            hit.description ? h('p', { class: 'small' }, hit.description) : null,
            hit.snippet ? h('p', { class: 'snippet small muted' }, hit.snippet) : null))));
}
