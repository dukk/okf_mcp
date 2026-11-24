// One folder of the tree: its breadcrumbs, its title (its index's title, or
// its name) and its index's description, an About row for the index's text,
// its subfolders (title, description, page count) and its concepts (title
// and description), then its log. Only the tree is read: About opens the
// index as a document.
import { h } from '/ui/js/core/dom.js';
import { read, enc } from '../api.js';
import { hrefs } from '../route.js';
import { folderListing, folderTitles } from '../listing.js';
import { icon } from '../icons.js';
import { crumbs, lede, pageTitle, row, sectionHead } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, folder } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/tree`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const documents = result.data.documents;
    const titles = folderTitles(documents);
    ctx.rememberTitles(titles);
    const listing = folderListing(documents, folder);
    const name = folder ? folder.split('/').pop() : ctx.label();
    // Conditional parts are appended one by one: the native append would
    // print a null.
    view.append(crumbs(catalog, folder, true, { root: ctx.label(), titles }),
        pageTitle(listing.index?.title ?? name, 'folder-title'));
    if (listing.index?.description) view.append(lede(listing.index.description));
    if (listing.index) {
        view.append(h('div', { class: 'rows' }, row({
            id: 'about', href: hrefs.document(catalog, listing.index.path),
            glyph: 'info', tint: 'accent', title: 'About', sub: 'What this folder is for',
        })));
    }
    if (listing.folders.length) {
        view.append(sectionHead('Folders'), h('div', { class: 'rows', id: 'folders' }, listing.folders.map(entry => row({
            href: hrefs.tree(catalog, entry.path), glyph: 'folder', tint: 'folder',
            title: entry.title ?? entry.name, sub: entry.description,
            end: [h('span', { class: 'r' }, String(entry.pages)), icon('chev', 'chev')],
        }))));
    }
    if (listing.concepts.length) {
        view.append(sectionHead('Pages', { count: listing.concepts.length }),
            h('div', { class: 'rows', id: 'concepts' }, listing.concepts.map(doc => row({
                href: hrefs.concept(catalog, doc.path), glyph: 'doc', title: listing.label(doc), sub: doc.description,
            }))));
    }
    if (listing.log) {
        view.append(h('div', { class: 'rows' }, row({
            id: 'folder-log', href: hrefs.document(catalog, listing.log.path),
            glyph: 'log', title: 'Log', sub: 'What changed in this folder',
        })));
    }
}
