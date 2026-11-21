// The catalog picker. With no catalog chosen yet and only one reachable,
// it chooses that one and goes home.
import { h } from '/ui/js/core/dom.js';
import { readAll } from '../api.js';
import { hrefs } from '../route.js';
import { message } from '../parts.js';

export async function render(view, ctx) {
    const result = await readAll('/catalogs', 'catalogs');
    if (!ctx.live()) return;
    ctx.freshness(result);
    const found = result.data;
    if (found.length === 1 && !ctx.state.catalog && !ctx.route.gone) {
        ctx.choose(found[0].name);
        location.replace(hrefs.home());
        return;
    }
    view.append(h('h1', {}, 'Catalogs'));
    if (ctx.route.gone) {
        view.append(message(`You can no longer read ${ctx.route.gone}. Its offline copies are deleted.`, null, 'catalog-gone'));
    }
    if (!found.length) {
        view.append(message('Your account reaches no catalog.', null, 'no-catalogs'));
        return;
    }
    view.append(h('ul', { class: 'list', id: 'catalog-list' }, found.map(catalog => h('li', {},
        h('button', {
            type: 'button', class: 'row', dataset: { catalog: catalog.name },
            onclick: () => { ctx.choose(catalog.name); location.hash = hrefs.home(); },
        },
        catalog.label ?? catalog.name,
        catalog.description ? h('span', { class: 'meta' }, catalog.description) : null)))));
}
