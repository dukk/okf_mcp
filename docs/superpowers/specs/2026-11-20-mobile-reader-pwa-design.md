# Mobile reader PWA: design

Date: 2026-11-20. Status: approved in brainstorming, awaiting implementation plan.

## Goal

A read-only, installable web app for reading a catalog on a phone (iPhone
through Safari's Add to Home Screen, Android through Chrome's install prompt),
with no AI in the loop. It runs on the existing server, on the same origin as
the web console, and reuses the console's session sign-in.

## Decisions

| Question | Decision |
|---|---|
| Native (Flutter + Rust) or PWA | PWA: one codebase, no app-store distribution, no new auth. |
| Offline | Recently viewed concepts are readable offline (bounded cache). No whole-catalog sync. |
| Where it lives | A dedicated reader at `/read/`, separate from the console at `/ui/`, with its own manifest and service worker scoped to `/read/`. |
| Session end | A `401` wipes the reader's caches and goes to sign-in; a lost catalog grant evicts only that catalog; offline, cached content stays readable. |
| Screens | Catalog picker, home (search and recent changes), search results, concept view with backlinks, tree browse, account. |
| Offline search | None. Offline, search says it needs a connection; home lists what is available offline. |
| Styling | Independent of the console: Pico CSS plus the reader's own `read.css` and tokens. The console's `styles.css` is not loaded. System fonts. |
| Cache cap | 200 concepts, least recently viewed evicted first. |

## Out of scope (MVP)

Editing; history and diffs; open questions and provenance detail; offline
search; whole-catalog sync; push notifications; background sync; catalog
unlock inside the reader (the reader links to the console for it); API-key
sign-in (the reader is cookie-only).

## Architecture

### Serving

- The reader's files live under `src/http/read/`, a sibling of the
  console's `src/http/ui/`. `build.rs` embeds each tree into its own table
  (`ASSETS` for the console, `READ_ASSETS` for the reader) with one shared
  function, so neither route can serve the other's files.
- The shell version is a hash of every file the worker precaches: the
  reader's own and the shared console files (`/ui/vendor/pico.min.css`,
  `/ui/js/core/dom.js`, `/ui/js/core/format.js`), computed once in
  `src/http/read.rs`. A change to any of them changes the version.
- A new module `src/http/read.rs` serves `/read` (permanent redirect to
  `/read/`), `/read/` (`index.html`), `/read/shell.json` (the version and the
  files to precache) and `/read/{*path}`, behind the console's
  `security_headers` layer and CSP. `src/http/ui.rs`'s `asset()` takes the
  table to serve from, so both modules share it.
- `/read/sw.js` is served with `Cache-Control: no-cache` and with
  `__SHELL_VERSION__` replaced by the shell version, so any change to a
  precached file changes the worker's bytes and installed apps update on
  their next launch. Its location makes its scope exactly `/read/`; it never
  controls a console page.
- `content_type()` gains `png` (`image/png`) and `webmanifest`
  (`application/manifest+json`).
- `is_safe_return_to` in `src/http/pages.rs` also accepts paths starting
  `/read/`, so sign-in can return to the reader. The existing visible-ASCII
  and no-backslash guards still apply.
- The existing CSP (`default-src 'self'`, `script-src 'self'`,
  `connect-src 'self'`) already permits a same-origin service worker and
  manifest; it is not loosened.

### Files under `src/http/read/`

- `index.html`: viewport with `viewport-fit=cover`, `<link rel="manifest">`,
  `apple-touch-icon`, `apple-mobile-web-app-capable` and status-bar meta,
  `theme-color` per colour scheme; loads `/ui/vendor/pico.min.css`,
  `read.css` and `js/main.js` (module). No web fonts.
- `manifest.webmanifest`: `name`, `short_name`, `start_url` and `scope`
  `/read/`, `display: standalone`, `background_color` and `theme_color`
  from the reader palette, icons 192 and 512 px (PNG, one `maskable`).
- `icons/`: `icon-192.png`, `icon-512.png`, `icon-maskable-512.png`,
  `apple-touch-icon.png` (180 px).
- `read.css`: the reader's own tokens on `:root` (overriding `--pico-*`
  variables for type scale, spacing, colours, radius, plus reader tokens for
  measure, touch-target size and safe-area padding), light and dark sets by
  `prefers-color-scheme`, phone-first layout, and rules for the HTML
  `src/render.rs` emits in `body_html` (plain markdown elements, and broken
  links marked by `title="Not in this catalog"`). That markup is the one
  contract shared with the console's stylesheet.
- `sw.js`: the service worker (caching, below).
- `js/main.js`: hash router and screen dispatch; registers `sw.js`.
- `js/refusals.js`: what a refused read does (effects, drawing, severity), without
  the DOM, so it is tested in Node.
- `js/api.js`: read-only client (below).
- `js/names.js`: a classic script setting `self.READ_CACHES` (the cache
  names), loaded by `index.html` and by `sw.js` through `importScripts`, so
  the names are written once.
- `js/cache.js`: page-side cache work through `window.caches` (the page and
  the worker share CacheStorage): wipe all, evict a catalog, evict a concept,
  list offline concepts, and the owner check.
- `js/screens/*.js`: one module per screen.

The reader imports `/ui/js/core/dom.js` (`h()`, `clear()`) and
`/ui/js/core/format.js` (`ago()`) from the console; neither imports anything. It does not import the console's
`api.js` (bearer keys, toasts, unlock dialog), `router.js` or
`components/tree.js` (which depends on the console router).

### Screens and routes

| Route | Screen | Data |
|---|---|---|
| `#/` | Home: search box and recent changes for the current catalog; offline, the "Available offline" list instead | `GET /api/v1/catalogs/{c}/recent-changes` |
| `#/c` | Catalog picker; skipped when the user reaches one catalog. The last choice is remembered in `localStorage` (wrapped in try/catch). | `GET /api/v1/catalogs` |
| `#/t/{catalog}/{folder}` | Tree: one folder's `index.md` rendered first (when it has one), then its subfolders and concepts. Logs and machinery files are not listed. | `GET /api/v1/catalogs/{c}/tree`, `GET .../documents/{folder}/index.md` |
| `#/s/{catalog}?q=` | Search results: title, path, snippet | `GET /api/v1/search?q=&catalogs={c}&limit=50` |
| `#/v/{catalog}/{path}` | Concept: title, breadcrumbs (tree folders), collapsible outline, `body_html`, a "Show the rest" action when the body is shortened (refetches with `?full=true`), and a "Linked from" list. A route with a section always asks for `?full=true`, as the console does, so outline links in a long concept reach their heading. | `GET /api/v1/catalogs/{c}/concepts/{path}`, `GET .../links/{path}` |
| `#/d/{catalog}/{path}` | A `log.md` or `AGENTS.md`, read through `/documents` (as the console does), so links to them from recent changes, backlinks and bodies are never dead | `GET .../documents/{path}` |
| `#/me` | Signed-in user, sign-out, and "Delete offline copies" (local, works offline) | `GET /api/v1/session` (user, `csrf`, `expires_at`; the `csrf` token is kept in memory only), `DELETE /api/v1/session` with the `X-OKF-CSRF` header |

Every link the reader makes goes through one rule: a folder's `index.md`
opens its folder, a `log.md` or `AGENTS.md` the document view, anything else
the concept view.

On every launch, boot waits (up to five seconds) for the worker to control
the page before the first read, so on a first visit the first screen's reads
already go through the worker and are cached. If registration fails or takes
longer, the reader runs online without offline copies.

Concept links in `body_html` are written by `src/render.rs` as
`#/browse/{catalog}/{path segments}[/{section slug}]`. The reader rewrites
them to `#/v/{catalog}/{path}[?s={slug}]`, or to the folder's tree route for a
link to an `index.md`. (Corrected for issue #812, 2026-11-20: a folder may be
named `x.md`, so the reader's route carries the section as a parameter, and
reads a console link's document as ending at its last `.md` segment, since a
heading slug has no `.`.) External links get `target="_blank" rel="noopener"`. `body_html`
is inserted with `innerHTML` exactly as the console does: it is produced and
sanitised server-side by `render.rs`.

## Caching (service worker)

Three caches:

1. `read-shell-<version>`: precached on `install` from `/read/shell.json`
   (every file under `src/http/read/` plus `/ui/vendor/pico.min.css`,
   `/ui/js/core/dom.js` and `/ui/js/core/format.js`). Cache-first; a
   navigation under `/read/` is answered with the cached `index.html`.
   `activate` deletes every `read-shell-*` cache except the current one and
   calls `clients.claim()`.
2. `read-content`: concept and document GETs
   (`/api/v1/catalogs/{c}/concepts/{path}` and `.../documents/{path}`, keyed
   on the full URL including `?full=true`). Network-first, cache
   fallback. (Brainstorming said stale-while-revalidate; network-first is
   used instead because answering from cache before the server has spoken
   would show a revoked session cached content while online, and it keeps
   one answer per request.) Capped at 200 entries, least recently viewed
   evicted first: every answer, fresh or cached, is re-put so it moves to the
   end of the cache's insertion order, and the oldest keys are deleted while
   more than 200 remain. No IndexedDB.
3. `read-meta`: the catalog list, trees, recent changes and backlinks.
   Network-first, cache fallback.
4. `read-owner`: one entry naming the user the caches belong to.

Invalidation marks live in the worker's memory, not in storage, so a full
disk never blocks a deletion. Before a wipe the page sends the worker an
"all" mark, before evicting a catalog that catalog's mark, and before
evicting one document (a `404`) that document's API path, and waits
(up to a second) for the worker to confirm before deleting. The worker does
not store an answer to a request that started at or before a matching mark,
and checks again after writing (deleting the entry if a mark appeared
meanwhile), so a slow `2xx` can never put back what a `401` or a lost grant
removed. A request on its way keeps the worker alive, so its marks last as
long as they matter. A takeover deletes the old copies and the old owner
record before it writes the new record, and keeps the new account's answers
only once that write succeeds, so the record never names an account other
than the one whose copies are cached. On a full disk the caches are wiped,
nothing is kept, and the reader boots anyway. Without Cache Storage
at all, or when it refuses to open or read (denied by browser settings),
the reader works online and keeps nothing: the worker passes answers
through without copies, and the page's cache calls log and return empty
instead of throwing, so boot, reads and deletion never fail on storage.

Offline, a concept request is also answered by its other cached form: a
`?full=true` copy answers a plain request, and a plain copy answers
`?full=true` when it holds the whole body (it was not shortened).

Never cached: search, `/api/v1/session`, `/api/v1/me`, every non-GET request, everything outside
`GET /api/v1` and the shell.

Cache writes are best effort: a refused write (QuotaExceededError) is
logged and the answer is still returned. Only `2xx` answers are cached; the worker stores each with an
`X-Read-Cached-At` header (RFC 3339). An answer served from cache because the
network failed also carries `X-Read-Offline: 1`. The worker never builds an
error page: it passes the network's answer through, answers from cache, or
lets the fetch fail.

Caches are tied to one account, decided by the worker, which every reader
tab shares. Every `/api/v1` answer carries `X-OKF-Account`, an opaque tag
(16 hex digits of SHA-256 of a prefixed user or key id). The first answer
for a new account wipes `read-content` and `read-meta` (after starting a new
ownership epoch, so answers still on their way for the old account drop
their copies, while the new account's keep theirs) and records the new
owner in `read-owner`; another account's answer to a request that started
before the current owner took over is never kept. So signing in as someone
else in another tab, with a reader tab left open, never mixes two accounts'
copies. Offline, a cached copy is served only to the account that owns the
copies: its own `X-OKF-Account` must be the owner's. With no owner (no
record, a wipe, or a takeover that could not delete the old copies),
nothing is served and the leftover copies are deleted, and the page's
"Available offline" list shows only the recorded owner's copies.
Sign-out wipes everything before calling `DELETE /api/v1/session`, and
again once the server confirms it, so a read admitted while the `DELETE`
was pending leaves nothing behind.
Sign-out, "Delete offline copies" and a `401` also forget the remembered
catalog, so the next person to sign in on the device never sees the last
one's catalog name. While a sign-out (or a 401's way to sign-in) is under
way, no catalog the user opens is remembered.

The shell is installed only when it is entirely one build's:
`/read/shell.json` lists each file with the SHA-256 of the bytes this build
serves, and the worker checks that the listing's version is its own and
that every file it fetched hashes as listed. A deploy between its requests,
or replicas on two builds, fails the install; the browser keeps the worker
it had and tries again on a later visit.

## Error handling

`js/api.js` classifies every answer; only the page decides what to do, in
one handler that every read goes through (a screen's first render and later
reads such as "Show the whole concept"). The cache and session side effects
(wipe, evict, sign-in) always happen, even when the answer arrives after the
user has left the screen that asked; only the drawing is skipped then. The
catalog a refusal applies to is the one the screen asked about, fixed when
the screen starts, never the current choice when the answer arrives. When
one screen makes several reads (a concept and its links), each refusal's
effects (wipe, evict, sign-in) run the moment that read is refused, without
waiting for the others; once all have settled, the most serious refusal
draws.

| Answer | Action |
|---|---|
| `2xx` | Render. With `X-Read-Offline`, show "Offline, synced <ago of X-Read-Cached-At>". |
| `401` | Wipe `read-content` and `read-meta` and forget the remembered catalog; navigate to `/signin?return_to=/read/` plus the current hash. |
| `403` with body `error` `Forbidden`, or `404` with `CatalogNotSelected` or `UnknownName` (a catalog no longer reachable) | Evict that catalog's entries in both caches and the remembered catalog; go to the picker with a short note. |
| `404` with `NotFound` on a concept | Evict that concept's entries; show "This concept no longer exists" and a link to its parent folder. |
| `404` with `NotFound` on a folder's `index.md` | Evict that document's entry and show the folder's lists without it. |
| `403` with body `error` `UnlockRequired` or `Suspended` | Keep the caches; show the server's message; for `UnlockRequired`, link to the console (`/ui/`) to unlock. |
| Network failure with no cached copy | "Not available offline" with a link to the Available Offline list. |

## Testing

Rust unit tests (`src/http/ui.rs`, `src/http/pages.rs`):

- `/read/`, `/read/sw.js`, `/read/manifest.webmanifest` and an icon are
  served with the right content types and the security headers.
- `/read/sw.js` carries `Cache-Control: no-cache` and the shell version,
  with no `__SHELL_VERSION__` left.
- The shell version changes when only a shared console file changes, and
  the served version covers the shared files.
- `/ui/sw.js` does not serve the reader's worker; `/read/styles.css` does not
  serve the console's stylesheet.
- `/read/shell.json` lists every reader file and the shared console files,
  each served with exactly the bytes its SHA-256 names.
- `X-OKF-Account` tags are stable per account, distinct between accounts,
  16 hex digits, and name no user.
- `safe_return_to` accepts `/read/...` and still rejects `//host`,
  backslashes and other prefixes.

Browser journeys (`tests/ui_e2e/read.rs`, rustwright headless Chromium):

1. Signed out, `/read/` goes to sign-in and returns to the same route.
2. Catalog picker, tree drill-down, concept, an internal link, "Linked from".
3. Search renders results; offline, search says it needs a connection.
4. View three concepts, go offline, reload: all three render with the synced
   note; an unviewed concept shows "Not available offline".
5. After viewing 201 concepts, the least recently viewed one is no longer
   cached and a re-viewed early one still is (no test hook: the journey
   fetches through the worker).
6. Revoke the session server-side: the next online request wipes the caches
   and goes to sign-in; offline afterwards shows nothing cached.
7. Remove the grant to catalog X: X's entries are evicted, catalog Y's
   survive.
8. `UnlockRequired`: caches survive; the unlock link points into the console.
8a. A lost grant answered after navigating to another screen still evicts
    that catalog, and the new screen stays.
8b. "Show the whole concept" after the grant is lost evicts the catalog.
8c. A late lost-grant answer for Home's catalog A, after switching to B,
    evicts A and keeps B chosen and cached.
8d. A concept 404 followed by a delayed 401 for its links still wipes and
    signs out.
8e. Offline, the account screen deletes the offline copies.
8f. With storage full, a concept still renders from the network.
8g. A successful answer held in the worker until after a lost-grant
    eviction does not put the catalog back.
8h. A folder whose `index.md` is deleted between the tree read and the index
    read still lists its folders and concepts, and the index's copy goes.
8i. In a long concept, expanding the body and following an outline link
    keeps the whole body and reaches the heading.
8j. With storage full and the owner record gone, the reader boots, deletes
    its offline copies and signs out.
8k. Offline, a concept opened plainly answers its outline links, and one
    opened through a section link answers a plain open.
8l. A success held in the worker until after its concept (or folder index)
    was deleted and evicted does not put it back.
8m. A concept opened once on a first visit (a direct link, no second online
    visit) is readable offline after a reload.
8n. A `log.md` in recent changes links to the document view, which renders.
8o. A concept read answered `401` while its links read is still held wipes
    and signs out without waiting for the links read.
8p. With every Cache Storage call rejected (page and worker), the reader
    boots, reads concepts and recent changes online, and deletion works.
8q. Signing in as a second account in another tab while a reader tab stays
    open: the next read wipes the first account's copies and records the
    second as owner.
8r. A new worker whose `read.css` answer is not the listed build's does not
    install (no shell cache, no control) and the reader still works online;
    with consistent files it installs.
9. Sign-out wipes the caches.
10. A new shell version replaces the old shell cache on activate.

Manual, before release: install on iPhone (Safari, Share, Add to Home Screen)
and on Android (Chrome install prompt); check standalone launch, safe areas
on a notched screen, light and dark, and offline reading in airplane mode.

## Known limits

- iOS may evict an installed web app's storage after weeks without use; the
  offline cache is best effort.
- An installed iOS web app has its own cookie store, so the user signs in
  once inside it (passkeys make that quick).
- A lost, unlocked phone shows cached concepts until it next reaches the
  server.
