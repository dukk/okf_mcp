// The one way pages talk to the server: /api/v1 with the stored bearer key,
// or else the console session's cookie (sent by the browser itself) with
// its CSRF token on writes. A non-2xx answer becomes an ApiError (already
// toasted unless `quiet`); a 401 also signs the browser out through the
// handler main.js installs.
import { store } from './store.js';
import { toast } from './toast.js';
import { h } from './dom.js';
// A cycle (unlock.js imports this module), safe because neither module calls
// the other while it loads, only later, when a refusal arrives.
import { canUnlock, openUnlock } from '../components/unlock.js';

export class ApiError extends Error {
    constructor(status, body) {
        super(body?.message ?? `HTTP ${status}`);
        this.status = status;
        this.body = body;
    }
}

let unauthorized = () => {};
export function setUnauthorizedHandler(handler) { unauthorized = handler; }

// The header carrying a console session's CSRF token on writes.
export const CSRF_HEADER = 'X-OKF-CSRF';

// The headers that authenticate one request to /api/v1, the one place
// that chooses them: a key as a bearer, or else nothing for the session
// cookie (the browser sends it by itself, same origin) plus the session's
// CSRF token on anything but a GET. `key` and `csrf` default to the stored
// ones; pass `key: null` for a request that must speak for the cookie alone.
export function authHeaders(method, { key = store.key, csrf = store.csrf } = {}) {
    if (key) return { Authorization: `Bearer ${key}` };
    if (method !== 'GET' && csrf) return { [CSRF_HEADER]: csrf };
    return {};
}

// The error kinds whose 403 shows the server's own message.
const OWN_MESSAGE = new Set(['Suspended', 'UnlockRequired']);

// A refusal's toast: its message and, for a catalog that needs an unlock and
// a credential that may ask for one, an Unlock action that opens the form.
function refusalMessage(error) {
    const catalog = error.body?.catalog;
    if (error.body?.error !== 'UnlockRequired' || !catalog || !canUnlock()) return error.message;
    return [error.message, ' ',
        h('button', { type: 'button', class: 'text-btn', onclick: () => openUnlock(catalog) }, 'Unlock')];
}

async function request(method, path, { body, raw = false, quiet = false, key } = {}) {
    const headers = authHeaders(method, { key: key ?? store.key });
    let payload;
    if (body instanceof Blob) {
        payload = body;
        headers['Content-Type'] = 'application/gzip';
    } else if (body !== undefined) {
        payload = JSON.stringify(body);
        headers['Content-Type'] = 'application/json';
    }
    const response = await fetch(`/api/v1${path}`, { method, headers, body: payload });
    if (response.status === 401) {
        unauthorized();
        throw new ApiError(401, null);
    }
    if (!response.ok) {
        let errorBody = null;
        try { errorBody = await response.json(); } catch { /* not JSON */ }
        const error = new ApiError(response.status, errorBody);
        const denied = store.key ? 'Your key cannot do this.' : 'Your account cannot do this.';
        // Some 403s carry their own message: show it, not "cannot do this". A
        // suspended team's (the catalog, the team and the operator's
        // notice), a catalog that needs an unlock first, and the maintenance
        // runner on a catalog whose owners turned maintenance off.
        const generic = response.status === 403
            && !OWN_MESSAGE.has(errorBody?.error) && errorBody?.maintenance !== 'off';
        if (!quiet) toast(generic ? denied : refusalMessage(error), 'error');
        throw error;
    }
    if (raw) return response;
    return response.status === 204 ? null : response.json();
}

// Downloads a GET answer as a file with the console's own credential (the
// bearer key, or the session cookie), which a plain link cannot carry for a
// key. Refuses as `request` does (toasted unless `quiet`).
async function download(path, filename, opts) {
    const response = await request('GET', path, { ...opts, raw: true });
    const url = URL.createObjectURL(await response.blob());
    const link = h('a', { href: url, download: filename, hidden: true });
    document.body.append(link);
    link.click();
    link.remove();
    // Revoked on the next turn, once the browser has started the download.
    setTimeout(() => URL.revokeObjectURL(url), 0);
}

// The largest page a catalog listing serves (M18: GET /catalogs, /whoami
// and /labels answer one page at a time, with `next` beside the list).
const LISTING_PAGE = 1000;

// The query that asks `path` for one page of the largest size, after
// `cursor` (null for the first page).
function pageQuery(path, cursor = null) {
    const params = new URLSearchParams({ limit: String(LISTING_PAGE) });
    if (cursor !== null) params.set('after', cursor);
    return `${path}${path.includes('?') ? '&' : '?'}${params}`;
}

// GETs every page of a listing, following `next` as `after` until the last
// page, and answers the first page with `field` holding every page's items
// (an array is joined, an object's entries are merged). For the views that
// need the whole list at once (a picker, the catalog switcher, the labels
// table); a list a person scrolls uses Load more instead. Each page is one
// bounded request. Rejects as api.get does.
async function pages(path, field, opts) {
    const answer = await request('GET', pageQuery(path), opts);
    let cursor = answer.next ?? null;
    while (cursor !== null) {
        const page = await request('GET', pageQuery(path, cursor), opts);
        const items = page[field];
        answer[field] = Array.isArray(items) ? answer[field].concat(items) : { ...answer[field], ...items };
        // A cursor that does not move would never end: stop at it.
        const next = page.next ?? null;
        cursor = next === cursor ? null : next;
    }
    delete answer.next;
    return answer;
}

export const api = {
    get: (path, opts) => request('GET', path, opts),
    pages,
    download,
    post: (path, body, opts) => request('POST', path, { ...opts, body }),
    put: (path, body, opts) => request('PUT', path, { ...opts, body }),
    patch: (path, body, opts) => request('PATCH', path, { ...opts, body }),
    del: (path, opts) => request('DELETE', path, opts),
};

export const enc = encodeURIComponent;
