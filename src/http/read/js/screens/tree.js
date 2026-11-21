// One folder of the tree: its index.md rendered first (when it has one and
// it can be read), then its subfolders and its concepts.
import { h } from '/ui/js/core/dom.js';
import { read, enc, encPath, KIND } from '../api.js';
import { hrefs } from '../route.js';
import { folderListing } from '../listing.js';
import { evictDocument } from '../cache.js';
import { crumbs, renderBody } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, folder } = ctx.route;
    const base = `/catalogs/${enc(catalog)}`;
    const result = await read(`${base}/tree`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const listing = folderListing(result.data.documents, folder);
    view.append(crumbs(catalog, folder, true), h('h1', {}, folder ? folder.split('/').pop() : 'Contents'));
    if (listing.index) {
        try {
            const overview = await read(`${base}/documents/${encPath(listing.index.path)}`);
            if (!ctx.live()) return;
            ctx.freshness(overview);
            view.append(renderBody(overview.data.body_html));
        } catch (error) {
            // Offline without a copy, or deleted since the tree was read:
            // the lists below still work. A deleted index's copy goes.
            if (error.kind === KIND.CONCEPT_GONE) await evictDocument(catalog, 'documents', listing.index.path);
            else if (error.kind !== KIND.UNAVAILABLE) throw error;
        }
        if (!ctx.live()) return;
    }
    if (listing.folders.length) {
        view.append(h('h2', {}, 'Folders'), h('ul', { class: 'list', id: 'folders' }, listing.folders.map(name =>
            h('li', {}, h('a', { class: 'folder', href: hrefs.tree(catalog, folder ? `${folder}/${name}` : name) }, name)))));
    }
    if (listing.concepts.length) {
        view.append(h('h2', {}, 'Concepts'), h('ul', { class: 'list', id: 'concepts' }, listing.concepts.map(doc =>
            h('li', {}, h('a', { href: hrefs.concept(catalog, doc.path) }, listing.label(doc))))));
    }
}
