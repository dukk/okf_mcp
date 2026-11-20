// The reader's routes, in the URL hash, every segment percent-encoded:
//   #/                                    home
//   #/c[?gone=<catalog>]                  catalog picker
//   #/t/<catalog>[/<folder>...]           a folder of the tree
//   #/s/<catalog>[?q=<query>]             search
//   #/v/<catalog>/<path>.md[/<section>]   a concept, optionally at a heading
//   #/d/<catalog>/<path>                  a log.md or AGENTS.md (read as a document)
//   #/me                                  account
const dec = decodeURIComponent;
const enc = encodeURIComponent;

function segments(path) {
    return String(path ?? '').replace(/^\/+/, '').split('/').filter(Boolean);
}

export function parse(hash) {
    const [body, query = ''] = hash.replace(/^#/, '').split('?');
    const [screen = '', catalog = null, ...rest] = body.split('/').filter(Boolean).map(dec);
    const params = new URLSearchParams(query);
    switch (screen) {
        case 'c': return { screen: 'catalogs', gone: params.get('gone') };
        case 't': return { screen: 'tree', catalog, folder: rest.join('/') };
        case 's': return { screen: 'search', catalog, query: params.get('q') ?? '' };
        case 'v': {
            const end = rest.findIndex(segment => segment.endsWith('.md'));
            if (end < 0) return { screen: 'tree', catalog, folder: rest.join('/') };
            return { screen: 'concept', catalog, path: rest.slice(0, end + 1).join('/'), section: rest[end + 1] ?? null };
        }
        case 'd': return { screen: 'document', catalog, path: rest.join('/') };
        case 'me': return { screen: 'me' };
        default: return { screen: 'home' };
    }
}

export const hrefs = Object.freeze({
    home: () => '#/',
    catalogs: (gone = null) => (gone ? `#/c?gone=${enc(gone)}` : '#/c'),
    tree: (catalog, folder = '') => `#/t/${[catalog, ...segments(folder)].map(enc).join('/')}`,
    search: (catalog, query = '') => `#/s/${enc(catalog)}${query ? `?q=${enc(query)}` : ''}`,
    concept: (catalog, path, section = null) =>
        `#/v/${[catalog, ...segments(path), ...(section ? [section] : [])].map(enc).join('/')}`,
    document: (catalog, path) => `#/d/${[catalog, ...segments(path)].map(enc).join('/')}`,
    me: () => '#/me',
});

// The folder holding `path` ('a/b/c.md' -> 'a/b').
export function parentOf(path) {
    return segments(path).slice(0, -1).join('/');
}

// The files the server serves through /documents, not /concepts (as the
// console's browse.js).
export const DOCUMENTS = new Set(['index.md', 'log.md', 'AGENTS.md']);

// Where a document opens: a folder's index.md opens its folder, a log.md or
// AGENTS.md the document view, anything else the concept view. Every link
// the reader makes (recent changes, backlinks, search hits, body links,
// the offline list) goes through here.
export function hrefFor(catalog, path, section = null) {
    const parts = segments(path);
    const name = parts[parts.length - 1];
    if (name === 'index.md') return hrefs.tree(catalog, parts.slice(0, -1).join('/'));
    if (DOCUMENTS.has(name)) return hrefs.document(catalog, parts.join('/'));
    return hrefs.concept(catalog, parts.join('/'), section);
}

// A link src/render.rs wrote ('#/browse/<catalog>/<path>[/<section>]', the
// console's route) as the reader's route, or null for any other link.
export function fromBrowse(href) {
    if (!href.startsWith('#/browse/')) return null;
    const [catalog, ...rest] = href.slice('#/browse/'.length).split('/').map(dec);
    const end = rest.findIndex(segment => segment.endsWith('.md'));
    if (end < 0) return hrefs.tree(catalog, rest.join('/'));
    return hrefFor(catalog, rest.slice(0, end + 1).join('/'), rest[end + 1] ?? null);
}
