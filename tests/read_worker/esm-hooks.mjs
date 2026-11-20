// A module-load hook for the reader's page modules (src/http/read/js/*.js).
// They are ES modules, but the repository has no package.json, and Node
// loads a .js file without one as CommonJS (a newer Node guesses, with a
// warning). This hook loads them as ES modules; page.mjs registers it.
//
// It also maps the imports of a screen loaded with the `?doubles` query
// (tree.js, catalogs.js and their tests) to the test doubles in tree-doubles/, so the
// screen runs in Node with a fake DOM and controlled reads, with no change
// to the screen itself.
const PAGE = new URL('../../src/http/read/js/', import.meta.url).href;
const DOUBLES = new URL('./tree-doubles/', import.meta.url).href;
const DOUBLED = new Map([
    ['/ui/js/core/dom.js', 'dom.mjs'],
    ['../api.js', 'api.mjs'],
    ['../cache.js', 'cache.mjs'],
    ['../parts.js', 'parts.mjs'],
]);

export async function resolve(specifier, context, nextResolve) {
    const parent = context.parentURL ?? '';
    if (parent.startsWith(`${PAGE}screens/`) && parent.endsWith('?doubles') && DOUBLED.has(specifier)) {
        return { url: `${DOUBLES}${DOUBLED.get(specifier)}`, shortCircuit: true };
    }
    return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
    if (!url.startsWith(PAGE)) return nextLoad(url, context);
    return nextLoad(url, { ...context, format: 'module' });
}
