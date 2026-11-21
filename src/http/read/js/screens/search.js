// Search in the current catalog: the title, folder and a plain-text snippet
// of each hit, "Show more" when a page fills the limit, and the count
// ("50+ results" while there may be more). Each answer replaces the list:
// two reads may see different snapshots, so their hits are never joined by
// position. It needs the server: offline it says so and points at what is
// available offline.
import { h, fill } from '/ui/js/core/dom.js';
import { read, enc, KIND, ReadError } from '../api.js';
import { hrefs, hrefFor } from '../route.js';
import { searchForm, message, row } from '../parts.js';
import { docLabel, folderTrail } from '../text.js';

// Hits per "page", and the most one search returns (src/mcp/search.rs,
// MAX_LIMIT): /search has no cursor, so "Show more" asks for more at once.
const PAGE = 50;
const MOST = 200;

function hitRow(hit) {
    return row({
        href: hrefFor(hit.catalog, hit.path), glyph: 'doc',
        title: docLabel(hit), sub: folderTrail(hit.path) || null, note: hit.snippet_text,
    });
}

function counted(n, full) {
    if (full) return `${n}+ results`;
    return `${n} ${n === 1 ? 'result' : 'results'}`;
}

export async function render(view, ctx) {
    const { catalog, query } = ctx.route;
    view.append(searchForm(catalog, query));
    if (!query) return;
    const url = limit => `/search?q=${enc(query)}&catalogs=${enc(catalog)}&limit=${limit}`;
    let limit = PAGE;
    let result;
    try {
        result = await read(url(limit));
    } catch (error) {
        if (error.kind !== KIND.UNAVAILABLE) throw error;
        if (!ctx.live()) return;
        view.append(message('Search needs a connection.',
            h('a', { href: hrefs.home(), id: 'offline-list-link' }, 'See what is available offline'), 'search-offline'));
        return;
    }
    if (!ctx.live()) return;
    const count = h('p', { class: 'hint', id: 'search-count' });
    const hits = h('div', { class: 'rows', id: 'hits' });
    const more = h('button', { type: 'button', class: 'more-button', id: 'search-more' }, 'Show more');
    const capped = message('Refine the search to see results past these.', null, 'search-capped');
    // Draws `answer` in place of the list, keeping the scroll position (the
    // "Show more" button is not redrawn, so it keeps focus), and what may
    // come next. The count is this answer's.
    const draw = answer => {
        const found = answer.data.hits;
        const scrolled = window.scrollY;
        fill(hits, found.map(hitRow));
        window.scrollTo(0, scrolled);
        const full = found.length >= limit;
        count.textContent = counted(found.length, full);
        more.hidden = !full || limit >= MOST;
        capped.hidden = !(full && limit >= MOST);
    };
    more.addEventListener('click', async () => {
        more.disabled = true;
        limit = Math.min(limit + PAGE, MOST);
        try {
            const answer = await read(url(limit));
            if (!ctx.live()) return;
            document.getElementById('more-offline')?.remove();
            draw(answer);
        } catch (error) {
            if (!(error instanceof ReadError)) throw error;
            limit -= PAGE;
            if (error.kind !== KIND.UNAVAILABLE) {
                await ctx.fail(error);
                return;
            }
            if (ctx.live() && !document.getElementById('more-offline')) {
                more.after(message('More results need a connection.', null, 'more-offline'));
            }
        } finally {
            more.disabled = false;
        }
    });
    view.append(count, hits, more, capped);
    draw(result);
}
