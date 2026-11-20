// A module-load hook for the reader's page modules (src/http/read/js/*.js).
// They are ES modules, but the repository has no package.json, and Node
// loads a .js file without one as CommonJS (a newer Node guesses, with a
// warning). This hook loads them as ES modules; page.mjs registers it.
const PAGE = new URL('../../src/http/read/js/', import.meta.url).href;

export async function load(url, context, nextLoad) {
    if (!url.startsWith(PAGE)) return nextLoad(url, context);
    return nextLoad(url, { ...context, format: 'module' });
}
