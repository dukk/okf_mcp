// The shared view parts as plain objects (the real ones need a DOM), with
// the same arguments as src/http/read/js/parts.js.
const kids = (...children) => children.flat(Infinity).filter(child => child !== null && child !== undefined && child !== false);

export function crumbs(catalog, folder, isFolder = false, names = {}) {
    return { tag: 'nav', attrs: { class: 'crumbs' }, children: [], catalog, folder, isFolder, root: names.root ?? catalog, titles: names.titles ?? null };
}

export function renderBody(html, options = {}) {
    return { tag: 'article', attrs: { id: 'body' }, children: [], html, serif: options.serif ?? false };
}

export function message(text, action = null, id = 'notice') {
    return { tag: 'div', attrs: { id }, children: kids(text, action) };
}

export function row({ href = null, glyph = 'doc', tint = null, title, sub = null, note = null, end = null, id = null, cls = null }) {
    return { tag: href ? 'a' : 'div', attrs: { class: cls ? `row ${cls}` : 'row', href, id }, glyph, tint, children: kids(title, sub, note, end) };
}

export function sectionHead(title, { count = null, id = null } = {}) {
    return { tag: 'div', attrs: { class: 'sec', id }, children: [count === null ? title : `${title} · ${count}`] };
}

export function pill(text, kind = null) {
    return { tag: 'span', attrs: { class: kind ? `pill pill-${kind}` : 'pill' }, children: [text] };
}

export function pageTitle(text, id = null) {
    return { tag: 'h1', attrs: { class: 'page', id }, children: [text] };
}

export function lede(text, cls = 'lede') {
    return text ? { tag: 'p', attrs: { class: cls }, children: [text] } : null;
}
