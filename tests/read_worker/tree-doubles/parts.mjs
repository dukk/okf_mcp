// The shared view parts as plain objects (the real ones need a DOM).
export function crumbs(catalog, folder) {
    return { tag: 'nav', attrs: { class: 'crumbs' }, children: [], catalog, folder };
}

export function renderBody(html) {
    return { tag: 'article', attrs: { id: 'body' }, children: [], html };
}
