// Loads the reader's page modules (src/http/read/js/) in Node as the page
// does, as ES modules (see esm-hooks.mjs). `query` gives a fresh instance.
import { register } from 'node:module';

register('./esm-hooks.mjs', import.meta.url);

/** The module `name` (for example 'route.js'), imported once per `query`. */
export function page(name, query = '') {
    return import(`${new URL(`../../src/http/read/js/${name}`, import.meta.url).href}${query}`);
}
