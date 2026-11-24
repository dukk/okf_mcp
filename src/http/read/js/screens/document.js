// A folder's index.md (its About page), a log.md or AGENTS.md: breadcrumbs,
// its name and its rendered text.
import { read, enc, encPath } from '../api.js';
import { crumbs, pageTitle, renderBody } from '../parts.js';

const NAMES = Object.freeze({ 'index.md': 'About', 'log.md': 'Log' });

export async function render(view, ctx) {
    const { catalog, path } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/documents/${encPath(path)}`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    const file = path.split('/').pop();
    view.append(crumbs(catalog, path, false, { root: ctx.label(), titles: ctx.titles() }),
        pageTitle(NAMES[file] ?? file, 'document-title'),
        renderBody(result.data.body_html));
}
