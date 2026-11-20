// The reader's one way to the server: GET /api/v1 with the session cookie
// (the browser sends it, same origin). Every refusal becomes a ReadError
// whose `kind` says what it means for the reader (reader design, "Error
// handling"); main.js decides what to do with it.

export const KIND = Object.freeze({
    SIGNED_OUT: 'signed-out',
    CATALOG_GONE: 'catalog-gone',
    CONCEPT_GONE: 'concept-gone',
    LOCKED: 'locked',
    UNAVAILABLE: 'unavailable',
    FAILED: 'failed',
});

export class ReadError extends Error {
    constructor(kind, message, body = null) {
        super(message);
        this.kind = kind;
        this.body = body;
    }
}

export const enc = encodeURIComponent;

// A catalog path ('/a/b.md' or 'a/b.md') as encoded URL segments.
export function encPath(path) {
    return String(path).replace(/^\/+/, '').split('/').map(enc).join('/');
}

// What a refusal means, by status and the body's error kind (the server's
// status mapping is in src/http/mod.rs, AppError::status).
export function classify(status, body) {
    const kind = body?.error;
    if (status === 401) return KIND.SIGNED_OUT;
    if (status === 403 && (kind === 'UnlockRequired' || kind === 'Suspended')) return KIND.LOCKED;
    if (status === 403 && kind === 'Forbidden') return KIND.CATALOG_GONE;
    if (status === 404 && (kind === 'CatalogNotSelected' || kind === 'UnknownName')) return KIND.CATALOG_GONE;
    if (status === 404 && kind === 'NotFound') return KIND.CONCEPT_GONE;
    return KIND.FAILED;
}

// GET /api/v1<path>: { data, offline, cachedAt }, or a ReadError.
// `offline` is true when the worker answered from its cache because the
// network failed; `cachedAt` is when that copy was stored.
export async function read(path) {
    let response;
    try {
        response = await fetch(`/api/v1${path}`, { headers: { Accept: 'application/json' } });
    } catch {
        throw new ReadError(KIND.UNAVAILABLE, 'Not available offline.');
    }
    if (!response.ok) {
        // A refusal's body is optional: without one, its status still decides.
        const body = await response.json().catch(() => null);
        throw new ReadError(classify(response.status, body), body?.message ?? `HTTP ${response.status}`, body);
    }
    // A 2xx whose body cannot be read (the connection dropped after the
    // headers, or it is not JSON) is a failed read, never empty data.
    let body;
    try {
        body = await response.json();
    } catch {
        throw new ReadError(KIND.FAILED, 'The answer could not be read.');
    }
    return {
        data: body,
        offline: response.headers.has('X-Read-Offline'),
        cachedAt: response.headers.get('X-Read-Cached-At'),
    };
}

// Every page of a listing (`next` cursors, as the console's api.pages):
// { data: <all items of `field`>, offline, cachedAt } of the first page.
export async function readAll(path, field) {
    const first = await read(path);
    const items = [...(first.data[field] ?? [])];
    let cursor = first.data.next ?? null;
    while (cursor !== null) {
        const page = await read(`${path}${path.includes('?') ? '&' : '?'}after=${enc(cursor)}`);
        items.push(...(page.data[field] ?? []));
        // A cursor that does not move would never end: stop at it.
        const next = page.data.next ?? null;
        cursor = next === cursor ? null : next;
    }
    return { ...first, data: items };
}

// Ends the session (DELETE /api/v1/session with its CSRF token).
// Returns whether the server confirmed it; throws a ReadError when offline.
export async function signOut(csrf) {
    try {
        const response = await fetch('/api/v1/session', { method: 'DELETE', headers: { 'X-OKF-CSRF': csrf } });
        return response.ok || response.status === 401;
    } catch {
        throw new ReadError(KIND.UNAVAILABLE, 'Not available offline.');
    }
}
