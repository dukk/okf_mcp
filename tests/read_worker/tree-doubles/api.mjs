// The real error kinds and path helpers; `read` is the test's, through
// globalThis.treeWorld.
export { KIND, ReadError, enc, encPath } from '../../../src/http/read/js/api.js';

export function read(path) {
    return globalThis.treeWorld.read(path);
}

export function readAll(path, field) {
    return globalThis.treeWorld.readAll(path, field);
}
