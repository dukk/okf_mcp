// A log.md or AGENTS.md: breadcrumbs, its name and its rendered text.
import { h } from '/ui/js/core/dom.js';
import { read, enc, encPath } from '../api.js';
import { crumbs, renderBody } from '../parts.js';

export async function render(view, ctx) {
    const { catalog, path } = ctx.route;
    const result = await read(`/catalogs/${enc(catalog)}/documents/${encPath(path)}`);
    if (!ctx.live()) return;
    ctx.freshness(result);
    view.append(crumbs(catalog, path, false), h('h1', { id: 'document-title' }, path.split('/').pop()),
        renderBody(result.data.body_html));
}
